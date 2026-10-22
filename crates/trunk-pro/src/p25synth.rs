//! `trunk-pro tool p25synth <out.cu8|out.cf32> [--center Hz] [--rate Hz]
//! [--freq Hz] [--keys keys.json] [--mi HEX] [--voice call.frames.jsonl |
//! call.sdr] [--superframes 10] [--wav out.wav] [--nac 659] [--tg 1201]
//! [--source 1234567]` — a synthetic P25 Phase 1 transmission (HDU, LDU1 /
//! LDU2, TDU) on `--freq`, encrypted with the first key of `--keys` (the
//! keys file `replay --keys` reads), or clear without it. The voice is a
//! saved call's IMBE frames (`--voice`), or frames made up for the purpose.
//! `--wav` also writes the transmission as 48 kHz discriminator audio, the
//! input dsd-fme takes.
//!
//! The capture checks decryption end to end, through `replay --p25 --keys`,
//! and against other decoders that hold the same key (OP25, dsd-fme).

use std::fs::File;
use std::io::{BufWriter, Write};

use num_complex::Complex32;
use trunk_core::nxdn::synth::add_noise;
use trunk_core::p25::crypt::Lookup;
use trunk_core::p25::synth::{dibits, imbe, modulate, Tx};
use trunk_core::p25::voice::EncryptionSync;
use trunk_core::trunk::frames::Codec;

use crate::{die, Args};

pub fn run(a: &Args) {
    let path = a.positional.get(1).unwrap_or_else(|| die("tool p25synth: no output file"));
    let center = a.num("center", 851_000_000.0);
    let fs = a.num("rate", 2_400_000.0);
    let freq = a.num("freq", center + 100_000.0);
    let cipher = a.get("keys").map(|p| {
        let keys = trunk_app::keys::load(std::path::Path::new(p), None).unwrap_or_else(|e| die(&e)).ring;
        let k = keys.keys().first().unwrap_or_else(|| die(&format!("{p}: no keys")));
        let keyid = k.keyid.unwrap_or_else(|| die(&format!("{p}: the first key needs a keyId")));
        let Lookup::Key(alg, key) = keys.lookup(k.alg.algid(), keyid, None) else { die(&format!("{p}: the first key can't be used by key ID alone")) };
        let mi = match a.get("mi") {
            Some(h) => {
                let b: Vec<u8> = (0..h.len() / 2).map(|i| u8::from_str_radix(h.get(2 * i..2 * i + 2).unwrap_or(""), 16).unwrap_or_else(|_| die("--mi: hex"))).collect();
                let mut mi = [0u8; 9];
                mi[..b.len().min(8)].copy_from_slice(&b[..b.len().min(8)]);
                mi
            }
            None => [0x31, 0x4f, 0x02, 0xa7, 0x9c, 0x55, 0xe0, 0x18, 0],
        };
        (EncryptionSync { algid: alg.algid(), keyid, mi }, key.to_vec())
    });
    let voice: Vec<[u8; 88]> = match a.get("voice") {
        Some(p) => crate::tool::load_frames(p).into_iter().filter(|f| f.codec == Codec::Imbe).map(|f| f.bits[..].try_into().unwrap()).collect(),
        None => (0..18 * a.num("superframes", 10.0) as usize).map(imbe).collect(),
    };
    if voice.is_empty() {
        die("tool p25synth: --voice has no IMBE frames");
    }
    let tx = Tx { nac: a.num("nac", 0x293 as f64) as u16, talkgroup: a.num("tg", 1201.0) as u16, source: a.num("source", 1_234_567.0) as u32, cipher, voice };

    // Random symbols around it, as a channel's receivers would hear before
    // and after a transmission (they set their levels on them).
    let mut x = 7u32;
    let mut pad = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 30) as u8
            })
            .collect()
    };
    let mut d = pad(2400);
    d.extend(dibits(&tx.frames()));
    d.extend(pad(4800));

    if let Some(wav) = a.get("wav") {
        let mut ph = 0.0;
        let iq = modulate(&d, 48_000.0, 0.0, 1.0, &mut ph);
        // The discriminator: frequency, ±1800 Hz → ±0.5.
        let audio: Vec<f32> = iq.windows(2).map(|w| (w[1] * w[0].conj()).arg() * 48_000.0 / (2.0 * std::f32::consts::PI) / 3600.0).collect();
        std::fs::write(wav, trunk_core::wav::encode(&audio, 48_000)).unwrap_or_else(|e| die(&format!("{wav}: {e}")));
    }

    let mut ph = 0.0;
    let mut iq: Vec<Complex32> = modulate(&d, fs, freq - center, 0.25, &mut ph);
    let mut seed = 1;
    add_noise(&mut iq, 0.01, &mut seed);
    let mut w = BufWriter::new(File::create(path).unwrap_or_else(|e| die(&format!("{path}: {e}"))));
    if path.ends_with(".cf32") {
        for v in &iq {
            w.write_all(&v.re.to_le_bytes()).and_then(|_| w.write_all(&v.im.to_le_bytes())).unwrap();
        }
    } else {
        for v in &iq {
            w.write_all(&[(v.re * 127.5 + 127.5).clamp(0.0, 255.0) as u8, (v.im * 127.5 + 127.5).clamp(0.0, 255.0) as u8]).unwrap();
        }
    }
    w.flush().unwrap();
    let what = match &tx.cipher {
        Some((es, _)) => format!("{} key {:#06X}, MI {}", trunk_core::p25::crypt::algid_name(es.algid), es.keyid, es.mi.iter().map(|b| format!("{b:02X}")).collect::<String>()),
        None => "clear".into(),
    };
    eprintln!(
        "{path}: {:.1} s at {fs} S/s, centre {center} Hz; P25 on {freq} Hz, NAC {:#05x}, TG {}, {} superframes, {what}",
        iq.len() as f64 / fs,
        tx.nac,
        tx.talkgroup,
        tx.superframes()
    );
}
