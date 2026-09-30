//! `trunk-pro tool dmrscan <capture> --center Hz --rate Hz [--seconds N]` —
//! every 6.25 kHz channel of a capture through the DMR receiver and framer:
//! the channels with DMR on them, by sync count, family and colour code.
//!
//! `trunk-pro tool dmr <capture> --center Hz --rate Hz --freq Hz [--bursts]
//! [--audio out.f32 --slot 0|1]` — one DMR channel: its link control, CSBKs
//! and calls as JSON lines (`--bursts`: every burst), then counts;
//! `--audio` vocodes a slot's voice (8 kHz f32).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;

use trunk_core::dmr::slot::data_type_name;
use trunk_core::dmr::{self, Burst, Framer, LcFrom, SlotEvent};
use trunk_core::dsp::c4fm::C4fm;
use trunk_core::dsp::{Channelizer, Receiver};
use trunk_core::mbe::{self, FRAME_SAMPLES};

use crate::{die, Args};

fn capture(a: &Args, what: &str) -> (Vec<u8>, f64, f64) {
    let path = a.positional.get(1).unwrap_or_else(|| die(&format!("tool {what}: no capture")));
    let mut cap = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let fs = a.num("fs", a.num("rate", 2_400_000.0));
    if let Some(s) = a.get("seconds").and_then(|v| v.parse::<f64>().ok()) {
        cap.truncate((s * fs) as usize * 2);
    }
    (cap, fs, a.num("center", 0.0))
}

pub fn run_scan(a: &Args) {
    let (cap, fs, center) = capture(a, "dmrscan");
    let mut chz = Channelizer::new(fs, 24_000.0, 0.1);
    let rate = chz.output_rate();
    let span = fs / 2.0 * 0.9;
    let step = 6250.0;
    let first = ((center - span) / step).ceil() as i64;
    let last = ((center + span) / step).floor() as i64;
    struct Ch {
        hz: f64,
        head: trunk_core::dsp::HeadId,
        rx: C4fm,
        framer: Framer,
        chan: dmr::Channel,
        syncs: BTreeMap<&'static str, u64>,
        cc: BTreeMap<u8, u64>,
        lcs: u64,
        voice: u64,
    }
    let mut chans: Vec<Ch> = (first..=last)
        .map(|k| {
            let hz = k as f64 * step;
            let (head, _, _) = chz.add_head(hz - center, dmr::CHANNEL_CUTOFF_HZ, 0.0);
            Ch { hz, head, rx: C4fm::dmr(rate), framer: Framer::default(), chan: dmr::Channel::default(), syncs: BTreeMap::new(), cc: BTreeMap::new(), lcs: 0, voice: 0 }
        })
        .collect();
    let t0 = Instant::now();
    let (mut syms, mut bursts, mut ev) = (Vec::new(), Vec::new(), Vec::new());
    let mut off = 0;
    while off < cap.len() {
        let (used, ran) = chz.feed_u8(&cap[off..]);
        off += used;
        if !ran {
            if used == 0 {
                break;
            }
            continue;
        }
        for c in chans.iter_mut() {
            syms.clear();
            bursts.clear();
            c.rx.push(chz.output(c.head).unwrap(), &mut syms);
            for s in &syms {
                c.framer.push(s, &mut bursts);
            }
            for b in &bursts {
                if let Some(k) = b.sync {
                    *c.syncs.entry(k.name()).or_default() += 1;
                    if k.is_data() {
                        let (cc, _, e) = b.slot_type();
                        if e <= 1 {
                            *c.cc.entry(cc).or_default() += 1;
                        }
                    }
                }
                ev.clear();
                c.chan.burst(b, &mut ev);
                for (_, e) in &ev {
                    match e {
                        SlotEvent::Lc { .. } => c.lcs += 1,
                        SlotEvent::Voice { frame, .. } if frame.errs <= 2 => c.voice += 1,
                        _ => {}
                    }
                }
            }
        }
    }
    let secs = cap.len() as f64 / 2.0 / fs;
    eprintln!("{} channels, {secs:.1} s of capture in {:.1} s", chans.len(), t0.elapsed().as_secs_f64());
    chans.retain(|c| c.syncs.values().sum::<u64>() >= 4);
    chans.sort_by_key(|c| std::cmp::Reverse(c.syncs.values().sum::<u64>()));
    for c in &chans {
        let total: u64 = c.syncs.values().sum();
        let kinds: Vec<String> = c.syncs.iter().map(|(k, n)| format!("{k}:{n}")).collect();
        let cc = c.cc.iter().max_by_key(|(_, &n)| n).map_or("?".into(), |(c, _)| c.to_string());
        println!("{:.5} MHz  syncs {total:6}  cc {cc:>2}  lc {:5}  voice {:6}  {}", c.hz / 1e6, c.lcs, c.voice, kinds.join(" "));
    }
}

pub fn run(a: &Args) {
    let (cap, fs, center) = capture(a, "dmr");
    let freq = a.num("freq", 0.0);
    let all = a.flag("bursts");
    let want_slot = a.num("slot", 0.0) as u8;
    let mut chz = Channelizer::new(fs, 24_000.0, 0.1);
    let rate = chz.output_rate();
    let (head, _, _) = chz.add_head(freq - center, dmr::CHANNEL_CUTOFF_HZ, 0.0);
    let mut rx = C4fm::dmr(rate);
    let mut framer = Framer::default();
    let mut chan = dmr::Channel::default();
    let mut vocoders = [mbe::Decoder::new(mbe::lcg(1), mbe::Profile::Enhanced), mbe::Decoder::new(mbe::lcg(2), mbe::Profile::Enhanced)];
    let mut audio_out = a.get("audio").map(|p| BufWriter::new(File::create(p).unwrap_or_else(|e| die(&format!("{p}: {e}")))));
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let (mut syms, mut bursts, mut ev) = (Vec::new(), Vec::<Burst>::new(), Vec::new());
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let (mut vcw, mut vcw_errs, mut vcw_clean) = (0u64, 0u64, 0u64);
    // Per slot: the last LC printed (only changes are), voice frames since.
    let mut last_lc: [Option<dmr::Lc>; 2] = [None, None];
    let t0 = Instant::now();
    let mut off = 0;
    while off < cap.len() {
        let (used, ran) = chz.feed_u8(&cap[off..]);
        off += used;
        if !ran {
            if used == 0 {
                break;
            }
            continue;
        }
        syms.clear();
        bursts.clear();
        rx.push(chz.output(head).unwrap(), &mut syms);
        for s in &syms {
            framer.push(s, &mut bursts);
        }
        for b in &bursts {
            let t = b.sample / rate;
            ev.clear();
            chan.burst(b, &mut ev);
            let slot = ev.first().map(|e| e.0);
            if all {
                let (tact, _) = dmr::burst::cach(&b.cach);
                let mut line = format!("{{\"t\":{t:.4},\"sync\":\"{}\",\"sync_errs\":{}", b.sync.map_or("", |k| k.name()), b.sync_errs);
                if let Some(t) = tact {
                    let _ = write!(line, ",\"tact_slot\":{},\"busy\":{}", t.slot, t.busy);
                }
                if let Some(s) = slot {
                    let _ = write!(line, ",\"slot\":{s}");
                }
                if b.sync.is_some_and(|k| k.is_data()) {
                    let (cc, dt, e) = b.slot_type();
                    let _ = write!(line, ",\"cc\":{cc},\"data_type\":\"{}\",\"slot_type_errs\":{e}", data_type_name(dt));
                    if matches!(dt, 0..=7 | 11) {
                        // The block and its CRC-CCITT residual (0 = the CRC checks unmasked).
                        let blk = dmr::fec::bptc196_decode(&b.info196());
                        let crc = dmr::fec::crc_ccitt(&blk.bits[..80]) ^ blk.bits[80..].iter().fold(0u16, |v, &x| v << 1 | x as u16);
                        let _ = write!(line, ",\"block\":\"{}\",\"bptc_errs\":{},\"crc_residual\":\"{crc:04x}\"", hex(&dmr::fec::pack(&blk.bits)), blk.errs);
                    }
                } else if b.sync.is_none() {
                    let (cc, pi, lcss, e) = b.emb();
                    let _ = write!(line, ",\"emb_cc\":{cc},\"pi\":{pi},\"lcss\":{lcss},\"emb_errs\":{e}");
                }
                line.push('}');
                let _ = writeln!(out, "{line}");
            }
            for (s, e) in ev.drain(..) {
                match e {
                    SlotEvent::Voice { frame, .. } => {
                        vcw += 1;
                        vcw_errs += frame.errs as u64;
                        vcw_clean += (frame.errs <= 1) as u64;
                        *counts.entry(format!("voice_slot{s}")).or_default() += 1;
                        if s == want_slot {
                            if let Some(w) = audio_out.as_mut() {
                                let mut buf = [0f32; FRAME_SAMPLES];
                                vocoders[s as usize].ambe(&frame.bits, frame.errs, &mut buf);
                                mbe::to_unit(&mut buf);
                                for v in buf {
                                    let _ = w.write_all(&v.to_le_bytes());
                                }
                            }
                        }
                    }
                    SlotEvent::Lc { lc, from } => {
                        *counts.entry(format!("lc_{from:?}").to_lowercase()).or_default() += 1;
                        if from != LcFrom::Embedded || last_lc[s as usize] != Some(lc) {
                            let _ = writeln!(
                                out,
                                "{{\"t\":{t:.4},\"slot\":{s},\"lc\":\"{from:?}\",\"flco\":{},\"fid\":{},\"group\":{},\"target\":{},\"source\":{},\"emergency\":{},\"encrypted\":{},\"hex\":\"{}\"}}",
                                lc.flco(),
                                lc.fid(),
                                lc.voice_user().unwrap_or(false),
                                lc.target(),
                                lc.source(),
                                lc.emergency(),
                                lc.encrypted(),
                                hex(&lc.0)
                            );
                        }
                        last_lc[s as usize] = if from == LcFrom::Terminator { None } else { Some(lc) };
                    }
                    SlotEvent::Csbk { csbk, mbc } => {
                        *counts.entry("csbk".into()).or_default() += 1;
                        let _ = writeln!(out, "{{\"t\":{t:.4},\"slot\":{s},\"csbk\":{},\"fid\":{},\"mbc\":{mbc},\"hex\":\"{}\"}}", csbk.opcode(), csbk.fid(), hex(&csbk.0));
                    }
                    SlotEvent::MbcContinuation(bytes) => {
                        let _ = writeln!(out, "{{\"t\":{t:.4},\"slot\":{s},\"mbc_continuation\":\"{}\"}}", hex(&bytes));
                    }
                    SlotEvent::Privacy { alg, key } => {
                        *counts.entry("pi_header".into()).or_default() += 1;
                        let _ = writeln!(out, "{{\"t\":{t:.4},\"slot\":{s},\"privacy\":{{\"alg\":{alg},\"key\":{key}}}}}");
                    }
                    SlotEvent::Data { data_type, ok } => {
                        *counts.entry(format!("{}{}", data_type_name(data_type), if ok { "" } else { "_bad" })).or_default() += 1;
                    }
                }
            }
        }
    }
    let _ = out.flush();
    let secs = cap.len() as f64 / 2.0 / fs;
    eprintln!("{secs:.1} s in {:.2} s; bursts {} syncs {} (framer locked: {}); CACH errors {}", t0.elapsed().as_secs_f64(), framer.bursts, framer.syncs, framer.locked(), chan.tact_errors);
    eprintln!("voice codewords {vcw}: {:.1} % with ≤ 1 error, {:.2} errors each", 100.0 * vcw_clean as f64 / vcw.max(1) as f64, vcw_errs as f64 / vcw.max(1) as f64);
    for (s, d) in chan.slots.iter().enumerate() {
        eprintln!("slot {s}: colour code {:?}, bad blocks {}", d.color_code, d.bad_blocks);
    }
    for (k, n) in counts {
        eprintln!("  {k}: {n}");
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
