//! A synthetic P25 transmitter, for tests and `tool p25synth`: a Phase 1
//! voice transmission (HDU, LDU1 / LDU2 superframes, TDU), clear or
//! encrypted with a known key, C4FM-modulated; and a Phase 2 one's voice
//! bursts as framed slot packets ([`Tx2`]). Every encoder here is the
//! inverse of a decoder in this module, and those were checked on real air
//! first, so a decode of its output checks the decryption end to end.
//!
//! Frames are built as op25's frame body (status symbols in place), the
//! layout the voice tables index; [`dibits`] gives what goes on the air.

use num_complex::Complex32;

use super::crypt::{next_mi, Algorithm, Keystream, Phase};
use super::fec::{golay24_encode, hamming1063_encode, rs_encode};
use super::frame::{encode_nid, frame_dibits, HDU, LDU1, LDU2, SYNC_BITS, TDU};
use super::phase2::{xor_mask, Packet, BURST_2V, BURST_4V, BURST_DIBITS, SLOT_DIBITS};
use super::voice::{bytes_to_hexbits, imbe_bits_to_params, imbe_header_encode, EncryptionSync};
use crate::ambe::encode_vcw;
use crate::dsp::c4fm::SYMBOL_RATE;
use crate::dsp::filters::rc;
use crate::tables::{DUID_LOOKUP, HDU_CODEWORD_BITS, ISCH_CODEWORDS, LDU_LS_DATA_BITS, VOICE_CODEWORD_BITS};

/// The status symbols' value (any will do: the decoders skip them).
const STATUS: u8 = 0b01;

/// A voice transmission.
#[derive(Clone, Debug)]
pub struct Tx {
    pub nac: u16,
    pub talkgroup: u16,
    pub source: u32,
    /// The cipher (ALGID, key ID, the first superframe's MI) and the key
    /// that encrypts the voice; None: clear.
    pub cipher: Option<(EncryptionSync, Vec<u8>)>,
    /// IMBE frames (88 bits, u0..u7 as mbelib's imbe_d) before encryption,
    /// filled out to whole superframes (18 frames) with zero frames.
    pub voice: Vec<[u8; 88]>,
}

impl Tx {
    pub fn superframes(&self) -> usize {
        self.voice.len().div_ceil(18).max(1)
    }

    /// The MI superframe `s` is encrypted under.
    pub fn mi(&self, s: usize) -> Option<[u8; 9]> {
        let (es, _) = self.cipher.as_ref()?;
        Some((0..s).fold(es.mi, |mi, _| next_mi(&mi)))
    }

    /// The frames, each as its dibits: HDU, LDU1 / LDU2 pairs, TDU.
    /// Panics on a key of the wrong length for its ALGID.
    pub fn frames(&self) -> Vec<Vec<u8>> {
        let es = |s: usize| match &self.cipher {
            Some((es, _)) => EncryptionSync { mi: self.mi(s).unwrap(), ..*es },
            None => EncryptionSync { algid: super::crypt::ALGID_CLEAR, keyid: 0, mi: [0; 9] },
        };
        let mut out = vec![hdu(self.nac, &es(0), self.talkgroup)];
        let svc = if self.cipher.is_some() { 0x40 } else { 0 };
        let lc = group_voice_lc(svc, self.talkgroup, self.source);
        for s in 0..self.superframes() {
            let ks = self.cipher.as_ref().map(|(e, key)| {
                let alg = Algorithm::from_algid(e.algid).expect("an ALGID the synth can encrypt with");
                Keystream::new(alg, key, &es(s).mi, Phase::One).expect("a key of the length its ALGID needs")
            });
            let ldu = |n: usize| -> [[u8; 88]; 9] {
                std::array::from_fn(|k| {
                    let mut bits = self.voice.get(18 * s + 9 * n + k).copied().unwrap_or([0; 88]);
                    if let Some(ks) = &ks {
                        ks.apply_p1(n, k, &mut bits);
                    }
                    bits
                })
            };
            out.push(ldu1(self.nac, &lc, &ldu(0)));
            out.push(ldu2(self.nac, &es(s + 1), &ldu(1)));
        }
        out.push(frame(self.nac, TDU, |_| {}));
        out
    }
}

/// A group voice channel user link control word (LCO 0).
pub fn group_voice_lc(svc_opts: u8, talkgroup: u16, source: u32) -> [u8; 9] {
    let [t0, t1] = talkgroup.to_be_bytes();
    let [_, s0, s1, s2] = source.to_be_bytes();
    [0, 0, svc_opts, 0, t0, t1, s0, s1, s2]
}

#[inline]
fn is_status(dibit: usize) -> bool {
    (dibit + 1) % 36 == 0
}

/// A frame of `duid`: sync, NID, status symbols, and a body `fill` writes
/// (op25's layout: bit positions from the sync's first bit, status symbols
/// in place). Returns its dibits.
fn frame(nac: u16, duid: u8, fill: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let n = frame_dibits(duid);
    let mut raw = vec![0u8; 2 * n];
    raw[..48].copy_from_slice(&SYNC_BITS);
    let nid = encode_nid(nac, duid);
    let (mut d, mut i) = (24, 0);
    while i < 64 {
        if !is_status(d) {
            raw[2 * d] = (nid >> (63 - i) & 1) as u8;
            raw[2 * d + 1] = (nid >> (62 - i) & 1) as u8;
            i += 2;
        }
        d += 1;
    }
    fill(&mut raw);
    (0..n).map(|d| if is_status(d) { STATUS } else { raw[2 * d] << 1 | raw[2 * d + 1] }).collect()
}

/// `n` bits of `v`, MSB first, at `raw[pos[0]], raw[pos[1]], …`.
fn put(raw: &mut [u8], pos: &[u16], v: u32) {
    let n = pos.len();
    for (i, &p) in pos.iter().enumerate() {
        raw[p as usize] = (v >> (n - 1 - i) & 1) as u8;
    }
}

/// An LDU's nine IMBE codewords and its 24 link-control / encryption-sync
/// hexbits (`hb[39..63]`, Hamming(10,6) each). The low-speed data is zero.
fn ldu(nac: u16, duid: u8, hb: &[u8; 63], voice: &[[u8; 88]; 9]) -> Vec<u8> {
    frame(nac, duid, |raw| {
        for (f, bits) in voice.iter().enumerate() {
            let cw = imbe_header_encode(&imbe_bits_to_params(bits));
            for (j, &b) in cw.iter().enumerate() {
                raw[VOICE_CODEWORD_BITS[f * 144 + j] as usize] = b;
            }
        }
        for i in 0..24 {
            put(raw, &LDU_LS_DATA_BITS[10 * i..10 * i + 10], hamming1063_encode(hb[39 + i]));
        }
    })
}

/// LDU1: link control, RS(24,12).
pub fn ldu1(nac: u16, lc: &[u8; 9], voice: &[[u8; 88]; 9]) -> Vec<u8> {
    let mut hb = [0u8; 63];
    hb[39..51].copy_from_slice(&bytes_to_hexbits(lc));
    rs_encode(&mut hb, 12);
    ldu(nac, LDU1, &hb, voice)
}

/// LDU2: the encryption sync for the next superframe, RS(24,16).
pub fn ldu2(nac: u16, es: &EncryptionSync, voice: &[[u8; 88]; 9]) -> Vec<u8> {
    let mut b = es.mi.to_vec();
    b.push(es.algid);
    b.extend(es.keyid.to_be_bytes());
    let mut hb = [0u8; 63];
    hb[39..55].copy_from_slice(&bytes_to_hexbits(&b));
    rs_encode(&mut hb, 8);
    ldu(nac, LDU2, &hb, voice)
}

/// HDU: MI, MFID 0, ALGID, key ID, talkgroup; RS(36,20), each hexbit
/// Golay(18,6).
pub fn hdu(nac: u16, es: &EncryptionSync, talkgroup: u16) -> Vec<u8> {
    let mut b = es.mi.to_vec();
    b.extend([0, es.algid]);
    b.extend(es.keyid.to_be_bytes());
    b.extend(talkgroup.to_be_bytes());
    let mut hb = [0u8; 63];
    hb[27..47].copy_from_slice(&bytes_to_hexbits(&b));
    rs_encode(&mut hb, 16);
    frame(nac, HDU, |raw| {
        for i in 0..36 {
            put(raw, &HDU_CODEWORD_BITS[18 * i..18 * i + 18], golay24_encode(hb[27 + i] as u32));
        }
    })
}

/// The frames back to back.
pub fn dibits(frames: &[Vec<u8>]) -> Vec<u8> {
    frames.concat()
}

/// C4FM at sample rate `fs`, `offset_hz` from the centre: ±600 / ±1800 Hz,
/// 4800 baud, each symbol a raised-cosine (α 0.2) frequency pulse — zero
/// intersymbol interference at the symbol centres, as the standard's
/// transmit filter (without its inverse-sinc shaping).
pub fn modulate(dibits: &[u8], fs: f64, offset_hz: f64, amp: f32, phase: &mut f64) -> Vec<Complex32> {
    let level = |d: u8| match d {
        0b01 => 3.0,
        0b00 => 1.0,
        0b10 => -1.0,
        _ => -3.0,
    };
    let sps = fs / SYMBOL_RATE;
    let n = (dibits.len() as f64 * sps) as usize;
    (0..n)
        .map(|i| {
            // Symbol k is centred at k + ½ symbols.
            let t = i as f64 / sps;
            let k0 = t as i64;
            let dev: f64 = (k0 - 8..=k0 + 8)
                .filter(|&k| k >= 0 && (k as usize) < dibits.len())
                .map(|k| 600.0 * level(dibits[k as usize]) * rc(t - k as f64 - 0.5, 0.2))
                .sum();
            *phase += 2.0 * std::f64::consts::PI * (offset_hz + dev) / fs;
            Complex32::from_polar(amp, *phase as f32)
        })
        .collect()
}

/// A recognisable IMBE frame: its bits vary with `n` (a decode of anything
/// is a frame to the vocoder; what matters is that it comes back the same).
pub fn imbe(n: usize) -> [u8; 88] {
    let mut x = 0x9e37_79b9u32 ^ (n as u32).wrapping_mul(0x85eb_ca6b);
    std::array::from_fn(|_| {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x & 1) as u8
    })
}

// ── Phase 2: one logical channel's bursts, as framed slot packets ────────────

/// A Phase 2 transmission on logical channel 0 (no MAC_PTT: a receiver
/// learns the cipher from the first superframe's ESS, as when it joins
/// late). Its 4V bursts are in superframe slots 0, 2, 4, 6 and the 2V in 8.
#[derive(Clone, Debug)]
pub struct Tx2 {
    /// The scrambler seed: NAC, System ID, WACN.
    pub nac: u32,
    pub sys_id: u32,
    pub wacn: u32,
    /// As [`Tx::cipher`].
    pub cipher: Option<(EncryptionSync, Vec<u8>)>,
    /// AMBE+2 frames (49 bits) before encryption, filled out to whole
    /// superframes (18 frames) with zero frames.
    pub voice: Vec<[u8; 49]>,
}

impl Tx2 {
    pub fn superframes(&self) -> usize {
        self.voice.len().div_ceil(18).max(1)
    }

    /// The MI superframe `s` is encrypted under.
    pub fn mi(&self, s: usize) -> Option<[u8; 9]> {
        let (es, _) = self.cipher.as_ref()?;
        Some((0..s).fold(es.mi, |mi, _| next_mi(&mi)))
    }

    /// The voice bursts as slot packets, each with its air time (s).
    pub fn packets(&self) -> Vec<(Packet, f64)> {
        let mask = xor_mask(self.nac, self.sys_id, self.wacn);
        let mut out = Vec::new();
        for s in 0..self.superframes() {
            let ks = self.cipher.as_ref().map(|(e, key)| {
                let alg = Algorithm::from_algid(e.algid).expect("an ALGID the synth can encrypt with");
                Keystream::new(alg, key, &self.mi(s).unwrap(), Phase::Two).expect("a key of the length its ALGID needs")
            });
            // This superframe's ESS: the sync of the next.
            let next = match &self.cipher {
                Some((es, _)) => EncryptionSync { mi: self.mi(s + 1).unwrap(), ..*es },
                None => EncryptionSync { algid: super::crypt::ALGID_CLEAR, keyid: 0, mi: [0; 9] },
            };
            let mut b = vec![next.algid];
            b.extend(next.keyid.to_be_bytes());
            b.extend(next.mi);
            let mut ess = [0u8; 63];
            ess[19..35].copy_from_slice(&bytes_to_hexbits(&b));
            rs_encode(&mut ess, 28);
            for burst in 0..5 {
                let n = if burst < 4 { 4 } else { 2 };
                let frames: Vec<[u8; 49]> = (0..n)
                    .map(|j| {
                        let f = 4 * burst + j;
                        let mut bits = self.voice.get(18 * s + f).copied().unwrap_or([0; 49]);
                        if let Some(ks) = &ks {
                            ks.apply_p2(f, &mut bits);
                        }
                        bits
                    })
                    .collect();
                let mut x = [0u8; BURST_DIBITS];
                let at: &[usize] = if burst < 4 { &[11, 48, 96, 133] } else { &[11, 48] };
                for (f, &a) in frames.iter().zip(at) {
                    x[a..a + 36].copy_from_slice(&encode_vcw(f));
                }
                let mut put_hexbit = |at: usize, h: u8| {
                    x[at..at + 3].copy_from_slice(&[h >> 4 & 3, h >> 2 & 3, h & 3]);
                };
                if burst < 4 {
                    for h in 0..4 {
                        put_hexbit(84 + 3 * h, ess[19 + 4 * burst + h]);
                    }
                } else {
                    let mut j = 84;
                    for i in 0..28 {
                        put_hexbit(j, ess[35 + i]);
                        j += if i == 15 { 4 } else { 3 };
                    }
                }
                let slot = 2 * burst;
                let mut p = Packet { sf_slot: slot, sample: 0.0, dibits: [0; SLOT_DIBITS], rel: vec![1.0; 2 * SLOT_DIBITS] };
                p.dibits[..20].copy_from_slice(&isch(slot));
                for i in 10..BURST_DIBITS {
                    p.dibits[10 + i] = x[i] ^ mask[slot * SLOT_DIBITS + i];
                }
                let kind = if burst < 4 { BURST_4V } else { BURST_2V };
                let duid = (0..=255u8).find(|&v| DUID_LOOKUP[v as usize] == kind).unwrap();
                for (k, pos) in [10, 47, 132, 169].into_iter().enumerate() {
                    p.dibits[10 + pos] = duid >> (6 - 2 * k) & 3;
                }
                out.push((p, 0.36 * s as f64 + 0.03 * slot as f64));
            }
        }
        out
    }
}

/// A slot's ISCH: the sync word in slots 2, 3, 6, 7, 10, 11, else an
/// I-ISCH (any: the tracker only needs to know it is one).
fn isch(slot: usize) -> [u8; 20] {
    let w = if matches!(slot % 4, 2 | 3) { 0x57_5D57_F7FF } else { ISCH_CODEWORDS.iter().find(|&&(_, v)| v >= 0).unwrap().0 };
    std::array::from_fn(|i| (w >> (38 - 2 * i) & 3) as u8)
}

/// A recognisable AMBE+2 frame, as [`imbe`].
pub fn ambe(n: usize) -> [u8; 49] {
    let b = imbe(n);
    std::array::from_fn(|i| b[i])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mbe;
    use crate::nxdn::synth::add_noise;
    use crate::p25::crypt::{Keyring, ALGID_ADP, ALGID_AES_128, ALGID_AES_256, ALGID_DES_OFB, ALGID_TDES};
    use crate::p25::frame::{decode_nid, Frame};
    use crate::p25::voice::{decode_hdu, decode_ldu1_lc, decode_ldu2_es, imbe_params_to_bits, ldu_imbe};
    use crate::trunk::voice::{build, CipherId, TrackerOut, VoiceKind, VoiceSpec};

    /// A frame's dibits as the framer hands it on (op25's raw body).
    fn raw(dibits: &[u8]) -> Frame {
        let raw: Vec<u8> = dibits.iter().flat_map(|d| [d >> 1, d & 1]).collect();
        let nid = raw.iter().enumerate().filter(|&(i, _)| !is_status(i / 2) && (48..).contains(&i)).take(64).fold(0u64, |a, (_, &b)| a << 1 | b as u64);
        Frame { nid: decode_nid(nid).expect("NID"), raw, complete: true, ..Default::default() }
    }

    fn es(algid: u8, keyid: u16) -> EncryptionSync {
        EncryptionSync { algid, keyid, mi: [0x31, 0x4f, 0x02, 0xa7, 0x9c, 0x55, 0xe0, 0x18, 0] }
    }

    fn tx(cipher: Option<(EncryptionSync, Vec<u8>)>, superframes: usize) -> Tx {
        Tx { nac: 0x293, talkgroup: 1201, source: 1_234_567, cipher, voice: (0..18 * superframes).map(imbe).collect() }
    }

    /// Each frame decodes to what went in: NID, HDU, link control,
    /// encryption sync (the next superframe's MI), and the IMBE words.
    #[test]
    fn frames_decode_as_built() {
        let t = tx(Some((es(ALGID_AES_256, 0x370E), vec![7; 32])), 3);
        let frames = t.frames();
        assert_eq!(frames.len(), 2 + 2 * 3);
        let lens: Vec<usize> = frames.iter().map(Vec::len).collect();
        assert_eq!(lens, [396, 864, 864, 864, 864, 864, 864, 72]);
        let f: Vec<Frame> = frames.iter().map(|d| raw(d)).collect();
        let duids: Vec<u8> = f.iter().map(|f| f.nid.duid).collect();
        assert_eq!(duids, [HDU, LDU1, LDU2, LDU1, LDU2, LDU1, LDU2, TDU]);
        assert!(f.iter().all(|f| f.nid.nac == 0x293 && f.nid.errors == 0));
        let h = decode_hdu(&f[0].raw).expect("HDU");
        assert_eq!((h.es, h.tgid, h.mfid), (es(ALGID_AES_256, 0x370E), 1201, 0));
        let lc = decode_ldu1_lc(&f[1].raw).expect("LC");
        assert_eq!((lc.lco, lc.svc_opts, lc.tgid, lc.source), (0, Some(0x40), Some(1201), Some(1_234_567)));
        for s in 0..3 {
            let e = decode_ldu2_es(&f[2 + 2 * s].raw).expect("ES");
            assert_eq!(e, EncryptionSync { mi: t.mi(s + 1).unwrap(), ..es(ALGID_AES_256, 0x370E) });
        }
        // The IMBE words are the ciphertext; clear, they're the voice.
        let t = tx(None, 1);
        let f: Vec<Frame> = t.frames().iter().map(|d| raw(d)).collect();
        for (n, fr) in f[1..3].iter().enumerate() {
            for (k, p) in ldu_imbe(fr, false).iter().enumerate() {
                assert_eq!(p.errs, 0);
                assert_eq!(imbe_params_to_bits(&p.u), t.voice[9 * n + k], "LDU{} frame {k}", n + 1);
            }
        }
        assert_eq!(decode_hdu(&f[0].raw).unwrap().es.algid, 0x80);
    }

    struct Heard {
        cipher: Vec<CipherId>,
        decrypted: usize,
        /// The audio frames' bits and erasure flags.
        voice: Vec<([u8; 88], bool)>,
    }

    /// `frames` through the Phase 1 voice decoder (C4FM receivers, framer,
    /// tracker) at 48 kHz, with `keys`, and Gaussian noise of `sigma`.
    fn hear(frames: &[Vec<u8>], keys: Option<Keyring>, sigma: f32) -> Heard {
        // Random symbols around it: the receivers need all four levels to
        // set their thresholds, and time to flush.
        let mut x = 5u32;
        let mut pad = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 30) as u8
                })
                .collect()
        };
        let mut d = pad(600);
        d.extend(dibits(frames));
        d.extend(pad(1200));
        let fs = 48_000.0;
        let mut ph = 0.0;
        let mut iq = modulate(&d, fs, 120.0, 1.0, &mut ph);
        let mut seed = 42;
        add_noise(&mut iq, sigma, &mut seed);
        let mut v = build(&VoiceSpec {
            kind: VoiceKind::Fdma,
            rate: fs,
            t0: 0.0,
            seed: 1,
            vocoder: mbe::Profile::Enhanced,
            bank: Default::default(),
            squelch: 0.0,
            subaudible: false,
            keys: keys.map(Arc::new),
        });
        let mut out = Vec::new();
        for c in iq.chunks(4800) {
            v.push(c, &mut out);
        }
        v.flush(&mut out);
        let mut h = Heard { cipher: Vec::new(), decrypted: 0, voice: Vec::new() };
        for o in out {
            match o.out {
                TrackerOut::Cipher(c) => h.cipher.push(c),
                TrackerOut::Decrypted => h.decrypted += 1,
                TrackerOut::Audio(_, f) => h.voice.push((f.bits.try_into().unwrap(), f.erased)),
                _ => {}
            }
        }
        h
    }

    fn keyring(algid: u8, keyid: u16, key: &[u8]) -> Keyring {
        let mut k = Keyring::default();
        k.add(algid, Some(keyid), key.to_vec(), vec![]).unwrap();
        k
    }

    /// Every cipher: encrypted on the air, decrypted by the whole voice
    /// path back to the bits that were sent, over several superframes (each
    /// LDU2's sync taking over for the next).
    #[test]
    fn each_cipher_decrypts_through_the_voice_path() {
        let key = |n: usize| (0..n).map(|i| (i * 37 + 11) as u8).collect::<Vec<u8>>();
        for (algid, len) in [(ALGID_ADP, 5), (ALGID_DES_OFB, 8), (ALGID_TDES, 24), (ALGID_AES_128, 16), (ALGID_AES_256, 32)] {
            let t = tx(Some((es(algid, 0x1B50), key(len))), 4);
            let h = hear(&t.frames(), Some(keyring(algid, 0x1B50, &key(len))), 0.0);
            assert_eq!(h.cipher, [CipherId { algid, keyid: 0x1B50 }], "{algid:#x}");
            assert_eq!(h.decrypted, 1, "{algid:#x}");
            assert_eq!(h.voice.len(), t.voice.len(), "{algid:#x}");
            for (i, (bits, erased)) in h.voice.iter().enumerate() {
                assert!(!erased && *bits == t.voice[i], "{algid:#x}: frame {i} isn't the clear voice");
            }
        }
    }

    /// The wrong key decrypts to something else; no key, nothing is heard.
    #[test]
    fn without_the_right_key_the_voice_stays_secret() {
        let t = tx(Some((es(ALGID_AES_256, 0x0009), vec![1; 32])), 2);
        let h = hear(&t.frames(), Some(keyring(ALGID_AES_256, 0x0009, &[2; 32])), 0.0);
        assert_eq!(h.voice.len(), t.voice.len());
        assert!(h.voice.iter().zip(&t.voice).all(|((b, _), v)| b != v));
        for keys in [None, Some(Keyring::default()), Some(keyring(ALGID_AES_256, 0x000A, &[1; 32]))] {
            let h = hear(&t.frames(), keys, 0.0);
            assert_eq!((h.decrypted, h.voice.len()), (0, 0));
            assert_eq!(h.cipher, [CipherId { algid: ALGID_AES_256, keyid: 0x0009 }]);
        }
    }

    /// An LDU2 whose encryption sync fails its FEC (its voice still good):
    /// the next superframe's MI follows from the LFSR, and nothing is lost.
    #[test]
    fn a_damaged_sync_is_predicted() {
        let t = tx(Some((es(ALGID_ADP, 0x1B50), vec![9, 8, 7, 6, 5])), 4);
        let mut frames = t.frames();
        // Two bits in each of 12 hexbits: Hamming(10,6) miscorrects them,
        // and RS(24,16) can't fix 12.
        let ldu2 = &mut frames[4];
        for i in 0..12 {
            for b in [10 * i, 10 * i + 1] {
                let p = LDU_LS_DATA_BITS[b] as usize;
                ldu2[p / 2] ^= 1 << (1 - p % 2);
            }
        }
        assert!(decode_ldu2_es(&raw(ldu2).raw).is_none());
        let h = hear(&frames, Some(keyring(ALGID_ADP, 0x1B50, &[9, 8, 7, 6, 5])), 0.0);
        assert_eq!(h.voice.len(), t.voice.len());
        assert!(h.voice.iter().zip(&t.voice).all(|((b, e), v)| !e && b == v));
    }

    /// An LDU2 lost to a fade takes its sync with it: the next superframe's
    /// LDU1 is held for the LDU2 after it, whose sync run back is its own.
    /// Only the lost LDU2's voice is missing.
    #[test]
    fn a_lost_ldu2_costs_only_its_own_voice() {
        let t = tx(Some((es(ALGID_AES_128, 0x4319), vec![3; 16])), 4);
        let mut frames = t.frames();
        // Superframe 1's LDU2 (frame 4) replaced by noise.
        let mut x = 77u32;
        for d in frames[4].iter_mut() {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            *d = (x >> 30) as u8;
        }
        let h = hear(&frames, Some(keyring(ALGID_AES_128, 0x4319, &[3; 16])), 0.0);
        let want: Vec<&[u8; 88]> = t.voice.iter().enumerate().filter(|(i, _)| !(27..36).contains(i)).map(|(_, v)| v).collect();
        assert_eq!(h.voice.len(), want.len());
        assert!(h.voice.iter().zip(want).all(|((b, e), v)| !e && b == v));
    }

    /// Joined late (the HDU and its sync missed): the first LDU1 is held
    /// for the LDU2, and the whole transmission comes through.
    #[test]
    fn a_late_join_decrypts_from_the_first_ldu() {
        let t = tx(Some((es(ALGID_ADP, 0x1B50), vec![1, 2, 3, 4, 5])), 3);
        let mut frames = t.frames();
        frames.remove(0);
        let h = hear(&frames, Some(keyring(ALGID_ADP, 0x1B50, &[1, 2, 3, 4, 5])), 0.0);
        assert_eq!(h.cipher, [CipherId { algid: ALGID_ADP, keyid: 0x1B50 }]);
        assert_eq!(h.voice.len(), t.voice.len());
        assert!(h.voice.iter().zip(&t.voice).all(|((b, e), v)| !e && b == v));
    }

    /// In noise (the FEC correcting about a bit per codeword), the voice
    /// that comes through is the clear voice: decryption adds no errors of
    /// its own.
    #[test]
    fn decrypts_in_noise() {
        let t = tx(Some((es(ALGID_DES_OFB, 0x0042), vec![0x13; 8])), 6);
        let h = hear(&t.frames(), Some(keyring(ALGID_DES_OFB, 0x0042, &[0x13; 8])), 0.45);
        assert_eq!(h.voice.len(), t.voice.len());
        let good = h.voice.iter().zip(&t.voice).filter(|((b, e), v)| !e && b == *v).count();
        assert!(good >= 100, "{good} of {} frames", t.voice.len());
    }
}
