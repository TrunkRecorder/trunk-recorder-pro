//! `trunk-pro tool tetrascan <capture> --center Hz [--fs Hz] [--seconds N] [--step 12500]` —
//! every channel of a capture through the TETRA receiver: the carriers with a cell's
//! identity on them, and what they carry.
//!
//! `trunk-pro tool tetra <capture> --center Hz --freq Hz [--messages] [--speech prefix]` — one
//! TETRA carrier: its cell, its calls (`--messages`: every MAC-RESOURCE), then counts.
//! `--speech`: each call's speech frames to `<prefix>-<n>.chd`, the standard's layout (EN 300
//! 395-2 clause 8: a word that is 1 for a bad frame, then 137 of one bit each, little-endian).

use std::collections::{BTreeMap, BTreeSet};

use trunk_core::dsp::{Channelizer, HeadId};
use trunk_core::tetra::acelp::{Decoder, Tables};
use trunk_core::tetra::cell::{Carrier, Event};
use trunk_core::tetra::cmce::Pdu;
use trunk_core::tetra::speech::{Frame, FRAME_BITS};
use trunk_core::tetra::{CUTOFF_HZ, MIN_RATE};

use crate::toolkit::Capture;
use crate::{die, Args};

struct Ch {
    hz: f64,
    head: HeadId,
    rx: Carrier,
    events: Vec<Event>,
}

/// Decode channels at `freqs` through a whole capture.
fn decode(cap: &Capture, freqs: impl Iterator<Item = f64>, speech: bool) -> Vec<Ch> {
    let mut chz = Channelizer::new(cap.fs, MIN_RATE, 0.1);
    let rate = chz.output_rate();
    let mut chans: Vec<Ch> = freqs.map(|hz| Ch { hz, head: chz.add_head(hz - cap.center, CUTOFF_HZ, 0.0).0, rx: Carrier::new(rate), events: Vec::new() }).collect();
    chans.iter_mut().for_each(|c| c.rx.cell.speech = speech);
    cap.channelize(&mut chz, |chz| {
        for c in &mut chans {
            c.rx.push(chz.output(c.head).unwrap(), &mut c.events);
        }
    });
    chans
}

/// A carrier's counts, as a line.
fn summary(c: &Ch) -> String {
    let n = c.rx.cell.counts;
    format!(
        "slots {}/{} locks {} bsch {}/{} blocks {}/{} aach {}/{} traffic {} offset {} Hz phase error {}°",
        c.rx.downlink.framer.good, c.rx.downlink.framer.good + c.rx.downlink.framer.bad, c.rx.downlink.framer.locks, n.bsch, n.bsch + n.bsch_bad, n.blocks, n.blocks + n.blocks_bad, n.aach, n.aach + n.aach_bad, n.traffic,
        c.rx.offset_hz().map_or("?".into(), |v| format!("{v:.0}")), c.rx.phase_error_deg().map_or("?".into(), |v| format!("{v:.1}")),
    )
}

fn cell(c: &Ch) -> String {
    let id = c.rx.cell.identity().map_or("no cell".into(), |s| format!("MCC {} MNC {} colour {}", s.mcc, s.mnc, s.colour));
    let info = c.rx.cell.sysinfo().map_or(String::new(), |s| {
        format!(" LA {} main carrier {} SCCHs {} air encryption {}", s.location_area, s.main_hz().map_or("?".into(), |h| format!("{:.4}", h / 1e6)), s.secondary, if s.encryption() { "offered" } else { "no" })
    });
    id + &info
}

pub fn run_scan(a: &Args) {
    let cap = Capture::from_args_seconds(a, "tetrascan");
    let span = trunk_core::trunk::usable_half_width(cap.fs, a.num("guard", trunk_core::trunk::DEFAULT_GUARD_HZ));
    let step = a.num("step", 12_500.0);
    let (first, last) = (((cap.center - span) / step).ceil() as i64, ((cap.center + span) / step).floor() as i64);
    let chans = decode(&cap, (first..=last).map(|k| k as f64 * step), false);
    println!("{} channels, {:.1} s of air", chans.len(), cap.air());
    for c in chans.iter().filter(|c| c.rx.cell.identity().is_some() || c.rx.downlink.framer.good > 50) {
        println!("{:.4} MHz  {}\n    {}", c.hz / 1e6, cell(c), summary(c));
    }
}

pub fn run(a: &Args) {
    let cap = Capture::from_args_seconds(a, "tetra");
    let hz = a.num("freq", 0.0);
    if hz == 0.0 {
        die("tool tetra: --freq Hz (--center Hz)");
    }
    let chans = decode(&cap, std::iter::once(hz), a.get("speech").is_some());
    let c = &chans[0];
    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    let mut to: BTreeSet<u32> = BTreeSet::new();
    let (mut clear, mut encrypted) = (0u64, 0u64);
    // (A slot's usage marker names a call while it lasts.)
    let mut speech: BTreeMap<(u8, u8), Vec<Option<Frame>>> = BTreeMap::new();
    let info = c.rx.cell.sysinfo();
    for e in &c.events {
        match e {
            Event::Cell(s) => println!("cell {s:?}"),
            Event::Sysinfo(s) => println!("sysinfo {s:?}"),
            Event::Traffic { .. } => {}
            Event::Speech { time, marker, frames } => speech.entry((time.slot, *marker)).or_default().extend(frames),
            Event::Signalling { time, sdu, pdu } => {
                if sdu.encryption != 0 { encrypted += 1 } else { clear += 1 }
                to.extend(sdu.to.ssi.filter(|_| sdu.encryption == 0));
                let kind = match pdu {
                    Some(Pdu::Call(call)) => format!("call {:?}", call.kind),
                    Some(Pdu::Mobility(m)) => format!("Mobility({})", m.kind),
                    Some(p) => format!("{p:?}"),
                    None if sdu.encryption != 0 => "encrypted".into(),
                    None => "other".into(),
                };
                *kinds.entry(kind).or_default() += 1;
                if a.flag("messages") || matches!(pdu, Some(Pdu::Call(_))) || sdu.allocation.is_some() {
                    let channel = sdu.allocation.map_or(String::new(), |al| {
                        let hz = info.and_then(|i| al.hz(&i)).map_or("?".into(), |h| format!("{:.4}", h / 1e6));
                        format!(" → {hz} slots {:?}", al.timeslots().collect::<Vec<_>>())
                    });
                    println!("{:02}/{:02}/{} {:?}{channel} {pdu:?}", time.multiframe, time.frame, time.slot, sdu.to);
                }
            }
        }
    }
    println!("{:.4} MHz  {}\n    {}", hz / 1e6, cell(c), summary(c));
    println!("    {clear} clear, {encrypted} encrypted; {} addresses; {kinds:?}", to.len());
    for (n, ((slot, marker), frames)) in speech.iter().enumerate() {
        println!("    speech {n}: slot {slot} marker {marker}: {} of {} frames good, {:.1} s", frames.iter().flatten().count(), frames.len(), frames.len() as f64 * 0.03);
        let words = frames.iter().flat_map(|f| std::iter::once(f.is_none() as u16).chain(f.unwrap_or([0; FRAME_BITS]).map(u16::from)));
        let bytes: Vec<u8> = words.flat_map(u16::to_le_bytes).collect();
        let path = format!("{}-{n}.chd", a.get("speech").unwrap_or_default());
        std::fs::write(&path, bytes).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    }
}

/// The codec's tables, from ETSI's archive for EN 300 395-2 (a .zip), or a folder holding that, or
/// the two files out of it ([`Tables::FILES`], either case). Nothing else in the archive is read.
pub fn tables(path: &std::path::Path) -> Result<Tables, String> {
    if path.is_file() {
        return tables_zip(path);
    }
    let zip = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip"))).min();
    let files = tables_files(path);
    match (files, zip) {
        (Err(_), Some(z)) => tables_zip(&z),
        (files, _) => files,
    }
}

/// The largest a table file in the archive may be, bytes (they are under 100 kB).
const TABLE_MAX: u64 = 1 << 20;

fn tables_zip(path: &std::path::Path) -> Result<Tables, String> {
    use std::io::Read;
    let bad = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = std::fs::File::open(path).map_err(|e| bad(&e))?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
    let mut read = |name: &str| -> Result<String, String> {
        let found = z.file_names().find(|n| n.rsplit(['/', '\\']).next().is_some_and(|f| f.eq_ignore_ascii_case(name))).map(str::to_string).ok_or_else(|| bad(&format!("no {name} in it")))?;
        let f = z.by_name(&found).map_err(|e| bad(&e))?;
        let mut data = Vec::new();
        f.take(TABLE_MAX + 1).read_to_end(&mut data).map_err(|e| bad(&e))?;
        if data.len() as u64 > TABLE_MAX {
            return Err(bad(&format!("{name} in it is too large")));
        }
        Ok(String::from_utf8_lossy(&data).into_owned())
    };
    let [lsp, gain] = Tables::FILES;
    Tables::parse(&read(lsp)?, &read(gain)?).map_err(|e| bad(&e))
}

fn tables_files(folder: &std::path::Path) -> Result<Tables, String> {
    let read = |name: &str| {
        let path = [name.to_string(), name.to_lowercase()].into_iter().map(|n| folder.join(n)).find(|p| p.exists()).ok_or(format!("{}: no {name}", folder.display()))?;
        // (Its comments may not be UTF-8.)
        std::fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("{}: {e}", path.display()))
    };
    let [lsp, gain] = Tables::FILES;
    Tables::parse(&read(lsp)?, &read(gain)?)
}

/// `trunk-pro tool tetravoice <in.chd> <out.pcm> --tetra-codec <folder or .zip>`: speech frames to 16-bit 8 kHz audio.
pub fn run_voice(a: &Args) {
    let [_, input, output] = &a.positional[..] else { die("tool tetravoice <in.chd> <out.pcm> --tetra-codec folder") };
    let t = tables(std::path::Path::new(a.get("tetra-codec").unwrap_or_else(|| die("--tetra-codec <folder or .zip>")))).unwrap_or_else(|e| die(&e));
    let mut d = Decoder::new(std::sync::Arc::new(t));
    let data = std::fs::read(input).unwrap_or_else(|e| die(&format!("{input}: {e}")));
    let pcm: Vec<u8> = data
        .as_chunks::<{ 2 * (1 + FRAME_BITS) }>().0.iter()
        .flat_map(|r| {
            let f: Frame = std::array::from_fn(|i| r[2 + 2 * i]);
            d.frame((r[0] == 0 && r[1] == 0).then_some(&f)).into_iter().flat_map(i16::to_le_bytes)
        })
        .collect();
    std::fs::write(output, pcm).unwrap_or_else(|e| die(&format!("{output}: {e}")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The archive's shape, made-up numbers in it.
    fn texts() -> [String; 2] {
        let list = |n: usize| (0..n).map(|i| (i % 900).to_string()).collect::<Vec<_>>().join(",");
        [
            format!("/* x */ static Word16 dico1_clsp[768] = {{ {} }};\nstatic Word16 dico2_clsp[1536] = {{ {} }};\nstatic Word16 dico3_clsp[2048] = {{ {} }};\n", list(768), list(1536), list(2048)),
            format!("static Word16 t_qua_ener[128] = {{ {} }};\n", list(128)),
        ]
    }

    #[test]
    fn tables_come_from_the_files_or_the_archive() {
        let dir = std::env::temp_dir().join(format!("trp-tetra-tables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::create_dir_all(dir.join("zip")).unwrap();
        let [lsp, gain] = texts();
        std::fs::write(dir.join("files/clsp_334.tab"), &lsp).unwrap();
        assert!(tables(&dir.join("files")).unwrap_err().contains("ENER_QUA.TAB"));
        std::fs::write(dir.join("files/ENER_QUA.TAB"), &gain).unwrap();
        let want = tables(&dir.join("files")).unwrap();
        let path = dir.join("zip/en_30039502v010303p0.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        for (name, text) in [("C-CODE/OTHER.C", "int x;"), ("C-CODE/CLSP_334.TAB", &lsp), ("C-CODE/ENER_QUA.TAB", &gain)] {
            z.start_file(name, o).unwrap();
            z.write_all(text.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        assert!(tables(&path).unwrap() == want && tables(&dir.join("zip")).unwrap() == want);
        // Another archive, and none.
        let other = dir.join("files/other.zip");
        zip::ZipWriter::new(std::fs::File::create(&other).unwrap()).finish().unwrap();
        assert!(tables(&other).unwrap_err().contains("no CLSP_334.TAB"));
        assert!(tables(&dir.join("nowhere")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
