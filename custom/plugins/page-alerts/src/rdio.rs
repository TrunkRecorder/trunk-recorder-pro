//! Sending a page's call to Rdio Scanner (`/api/call-upload`), as Tone Board
//! and Trunk Recorder's uploader do.

use std::path::Path;
use std::time::Duration;

use trunk_recorder_plugin::Multipart;

/// One upload of a call: where it goes and how it's labelled.
#[derive(Clone, Debug, PartialEq)]
pub struct Upload {
    pub system: u32,
    pub talkgroup: u32,
    pub label: String,
    pub source: Option<u32>,
    pub patches: Vec<u32>,
}

pub enum Failed {
    /// Worth trying again (the server's down or busy).
    Again(String),
    /// Refused: trying again won't help.
    Refused(String),
}

pub fn send(server: &str, key: &str, u: &Upload, audio: &Path, date_s: i64, freq_hz: u64) -> Result<(), Failed> {
    let bytes = std::fs::read(audio).map_err(|e| Failed::Refused(format!("{}: {e}", audio.display())))?;
    let name = audio.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "call.wav".into());
    let mime = if name.ends_with(".m4a") { "audio/mp4" } else { "audio/wav" };
    let mut m = Multipart::new()
        .text("key", key)
        .text("system", u.system.to_string())
        .text("talkgroup", u.talkgroup.to_string())
        .text("dateTime", date_s.to_string())
        .text("talkgroupLabel", &u.label)
        .text("talkgroupTag", "Page")
        .text("audioName", &name)
        .text("audioType", mime);
    if freq_hz > 0 {
        m = m.text("frequency", freq_hz.to_string());
    }
    if let Some(s) = u.source {
        m = m.text("source", s.to_string());
    }
    if !u.patches.is_empty() {
        m = m.text("patches", serde_json::to_string(&u.patches).unwrap_or_default());
    }
    let (body, ct) = m.file("audio", &name, mime, &bytes).finish();
    let url = format!("{}/api/call-upload", server.trim().trim_end_matches('/'));
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).http_status_as_error(false).build().new_agent();
    match agent.post(&url).header("Content-Type", &ct).send(&body[..]) {
        Ok(mut r) => {
            let code = r.status().as_u16();
            let text = r.body_mut().read_to_string().unwrap_or_default();
            match code {
                200..=299 => Ok(()),
                408 | 429 | 500..=599 => Err(Failed::Again(format!("Rdio Scanner answered {code}: {}", text.trim()))),
                401 | 403 => Err(Failed::Refused(format!("Rdio Scanner refused the API key ({code})"))),
                _ => Err(Failed::Refused(format!("Rdio Scanner refused it ({code}): {}", text.trim()))),
            }
        }
        Err(e) => Err(Failed::Again(format!("{url}: {e}"))),
    }
}
