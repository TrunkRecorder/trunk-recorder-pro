//! Signal taps saved to files: each stream the config's rows ask for
//! ([`trunk_app::config::TapRoute`]) gathered as its chunks come and, once it has ended,
//! written under the recordings folder ([`trunk_app::taps::TapFile`]: a `.wav` and its
//! sidecar, or a `.jsonl`).

use crate::folder::Folder;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use trunk_app::taps::TapFile;
use trunk_recorder_plugin::TapChunk;

/// The streams being gathered, each with its files' path (relative to the recordings
/// folder, without an extension).
#[derive(Default)]
pub struct TapFiles {
    open: Mutex<HashMap<u64, (String, TapFile)>>,
}

impl TapFiles {
    /// A chunk of a stream saved at `rel`: the stream and its path, when that was its last.
    pub fn gather(&self, rel: String, chunk: TapChunk) -> Option<(String, TapFile)> {
        let (id, last) = (chunk.stream, chunk.last);
        let mut open = self.open.lock().unwrap();
        if chunk.first {
            open.insert(id, (rel, TapFile::new(chunk)));
        } else if let Some((_, f)) = open.get_mut(&id) {
            f.push(chunk);
        }
        if last { open.remove(&id) } else { None }
    }

    /// A chunk of a stream saved at `rel`. Its last one writes the files, on a thread of
    /// their own (minutes of samples, at the longest).
    pub fn take(&self, folder: &Arc<Folder>, rel: String, chunk: TapChunk) {
        let Some((rel, file)) = self.gather(rel, chunk) else { return };
        let folder = folder.clone();
        let write = move || {
            for (ext, bytes) in file.files() {
                // (The folder says why, once, when it can't be written.)
                if folder.write(&folder.path().join(format!("{rel}.{ext}")), &bytes).is_err() {
                    break;
                }
            }
        };
        let _ = std::thread::Builder::new().name("tap-file".into()).spawn(write);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trunk_recorder_plugin::{Tap, TapData};

    #[test]
    fn a_stream_is_written_when_it_ends() {
        let dir = std::env::temp_dir().join(format!("trunk-pro-taps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folders = crate::folder::Folders::default();
        folders.recordings.set_path(&dir);
        let chunk = |seq: u32, last: bool| TapChunk {
            tap: Tap::FmDiscriminator,
            stream: 4,
            seq,
            first: seq == 0,
            last,
            offset: seq as u64 * 2,
            system: 0,
            system_id: 1,
            short_name: "wmata".into(),
            freq_hz: 496_337_500.0,
            slot: 0,
            call_id: None,
            talkgroup: None,
            unit: None,
            voice_mode: "fm".into(),
            decrypted: false,
            time: 1_800_000_000.0,
            data: TapData::Pcm { rate: 22_050, samples: vec![5, 6] },
            notes: Vec::new(),
        };
        let files = TapFiles::default();
        let rel = "wmata/2027/1/15/1800000000_496337500-tap_4.fm.discriminator";
        files.take(&folders.recordings, rel.into(), chunk(0, false));
        assert!(!dir.join(format!("{rel}.wav")).exists(), "not before it ends");
        files.take(&folders.recordings, rel.into(), chunk(1, true));
        let wav = dir.join(format!("{rel}.wav"));
        for _ in 0..200 {
            if wav.exists() && dir.join(format!("{rel}.jsonl")).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read(&wav).unwrap().len(), 44 + 8);
        let side = std::fs::read_to_string(dir.join(format!("{rel}.jsonl"))).unwrap();
        assert!(side.lines().next().unwrap().contains("\"items\":4"), "{side}");
        assert!(files.open.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
