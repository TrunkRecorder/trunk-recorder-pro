//! One voice channel's call tracker (Phase 1): HDU / LDU1 / LDU2 / TDU(LC)
//! frames → encryption and link-control state → IMBE vocoder → audio.
//! Takes receiver-diversity groups: header fields from the best NID, link
//! control / encryption sync from whichever copy decodes, each codeword from
//! the receiver the soft decoder trusted most.

use super::frames::{Codec, VoiceFrame};
use crate::mbe::{self, Kind, FRAME_SAMPLES};
use crate::p25::alias::{Alias, LcAliases};
use crate::p25::diversity::{best_es, best_frame, best_imbe, best_lc, Group};
use crate::p25::frame::{HDU, LDU1, LDU2, TDU, TDULC};
use crate::p25::voice::{decode_hdu, decode_tdulc, imbe_params_to_bits, ldu_codeword_end_bit, ALGID_CLEAR};

#[derive(Clone, Debug, PartialEq)]
pub enum TrackerOut {
    /// 20 ms of 8 kHz audio in [−1, 1], and the vocoder frame it came from.
    Audio(Vec<f32>, VoiceFrame),
    /// Link control named a source / emergency, or the call turned out encrypted.
    Info { source: Option<u32>, emergency: bool, encrypted: bool },
    /// Analog FM voice (a SmartNet analog channel): 8 kHz audio, squelched.
    AnalogAudio(Vec<f32>),
    /// A radio's talker alias, heard during the call.
    Alias(Alias),
}

/// Algorithm id when encryption is known but not which cipher.
const ALGID_UNKNOWN: i32 = -1;

pub struct VoiceTracker {
    dec: mbe::Decoder,
    active: bool,
    encrypted: bool,
    algid: i32,
    tgid: Option<u32>,
    /// The unit link control named for the current transmission.
    talker: Option<u32>,
    aliases: LcAliases,
    call_frames: u64,
    end_s: f64,
    /// Vocoder frames / repeated or muted, over the channel's life.
    pub frames: u64,
    pub bad_frames: u64,
}

impl VoiceTracker {
    pub fn new(rng: mbe::Rng, vocoder: mbe::Profile) -> Self {
        VoiceTracker {
            dec: mbe::Decoder::new(rng, vocoder),
            active: false,
            encrypted: false,
            algid: ALGID_CLEAR as i32,
            tgid: None,
            talker: None,
            aliases: LcAliases::default(),
            call_frames: 0,
            end_s: 0.0,
            frames: 0,
            bad_frames: 0,
        }
    }

    /// The talkgroup link control named for the current transmission.
    pub fn talkgroup(&self) -> Option<u32> {
        self.tgid
    }

    /// A frame group at time `t` (s, sample clock).
    pub fn group(&mut self, g: &Group, t: f64, out: &mut Vec<TrackerOut>) {
        let best = best_frame(g);
        match best.nid.duid {
            HDU => {
                let h = decode_hdu(&best.raw);
                self.algid = h.map_or(ALGID_CLEAR as i32, |h| h.es.algid as i32);
                self.start(t, h.map(|h| h.tgid as u32));
            }
            LDU1 | LDU2 => {
                self.touch(t);
                if best.nid.duid == LDU1 {
                    if let Some(lc) = best_lc(g) {
                        if lc.protected {
                            let a = if self.algid == ALGID_CLEAR as i32 { ALGID_UNKNOWN } else { self.algid };
                            self.set_encryption(a, out);
                        } else if lc.lco == 0 || lc.lco == 3 {
                            if let (Some(cur), Some(tg)) = (self.tgid, lc.tgid) {
                                if cur != tg && self.call_frames > 0 {
                                    self.start(t, None);
                                }
                            }
                            if lc.tgid.is_some() {
                                self.tgid = lc.tgid;
                            }
                            if lc.source.is_some_and(|s| s != 0) {
                                self.talker = lc.source;
                            }
                            let svc = lc.svc_opts.unwrap_or(0);
                            if svc & 0x40 != 0 && self.algid == ALGID_CLEAR as i32 {
                                self.set_encryption(ALGID_UNKNOWN, out);
                            }
                            out.push(TrackerOut::Info { source: lc.source, emergency: svc & 0x80 != 0, encrypted: self.encrypted });
                        } else if let Some(a) = self.aliases.lcw(&lc.raw, self.talker, self.tgid) {
                            out.push(TrackerOut::Alias(a));
                        }
                    }
                } else if let Some(es) = best_es(g) {
                    self.set_encryption(es.algid as i32, out);
                }
                // A cut frame (the next sync came early, in every receiver):
                // from the first codeword that fails its FEC or runs past the
                // cut, nothing is trusted — erasures.
                let complete = g.iter().any(|f| f.complete);
                let cut_at = (!complete).then(|| best.raw.len());
                let chosen = best_imbe(g);
                let mut erased = false;
                for (k, p) in chosen.iter().enumerate() {
                    if cut_at.is_some_and(|c| p.errs > 2 || ldu_codeword_end_bit(k) >= c) {
                        erased = true;
                    }
                    self.voice_frame(p.u, p.e0, p.errs, erased, out);
                }
            }
            TDU | TDULC => {
                // Link control from whichever receiver's copy decodes. Motorola
                // sends talker aliases here, in the terminators after the voice.
                let lc = g
                    .iter()
                    .filter(|f| f.nid.duid == TDULC)
                    .find_map(|f| decode_tdulc(&f.raw))
                    .filter(|lc| !lc.protected);
                if let Some(lc) = lc {
                    if self.active && self.tgid.is_none() {
                        self.tgid = lc.tgid;
                    }
                    if let Some(a) = self.aliases.lcw(&lc.raw, self.talker, self.tgid) {
                        out.push(TrackerOut::Alias(a));
                    }
                }
                self.active = false;
                self.algid = ALGID_CLEAR as i32;
            }
            _ => {}
        }
    }

    fn start(&mut self, t: f64, tgid: Option<u32>) {
        self.dec.reset();
        self.active = true;
        self.tgid = tgid;
        self.talker = None;
        self.encrypted = self.algid != ALGID_CLEAR as i32;
        self.end_s = t;
        self.call_frames = 0;
    }

    fn touch(&mut self, t: f64) {
        if self.active && t - self.end_s > 1.0 {
            // Quiet with no terminator: the next transmission re-learns its cipher.
            self.active = false;
            self.algid = ALGID_CLEAR as i32;
        }
        if !self.active {
            self.start(t, None);
        }
        self.end_s = t;
    }

    fn set_encryption(&mut self, algid: i32, out: &mut Vec<TrackerOut>) {
        self.algid = algid;
        if self.active && algid != ALGID_CLEAR as i32 && !self.encrypted {
            self.encrypted = true;
            out.push(TrackerOut::Info { source: None, emergency: false, encrypted: true });
        }
    }

    fn voice_frame(&mut self, u: [u32; 8], e0: u32, errs: u32, erased: bool, out: &mut Vec<TrackerOut>) {
        self.frames += 1;
        self.call_frames += 1;
        if self.encrypted {
            return;
        }
        let mut buf = [0f32; FRAME_SAMPLES];
        let bits = imbe_params_to_bits(&u);
        let kind = self.dec.imbe(&bits, e0, if erased { 0 } else { errs }, erased, &mut buf);
        if kind != Kind::Voice {
            self.bad_frames += 1;
        }
        mbe::to_limited(&mut buf);
        let frame = VoiceFrame { codec: Codec::Imbe, bits: bits.to_vec(), e0, errs, erased, kind };
        out.push(TrackerOut::Audio(buf.to_vec(), frame));
    }
}
