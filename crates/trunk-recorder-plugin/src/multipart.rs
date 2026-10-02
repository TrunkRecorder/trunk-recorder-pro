//! A `multipart/form-data` body — how most upload services take a call — for
//! any HTTP client:
//!
//! ```
//! # use trunk_recorder_plugin::Multipart;
//! let (body, content_type) = Multipart::new()
//!     .text("talkgroup_num", "101")
//!     .file("call", "101-1700000000.m4a", "audio/mp4", b"...")
//!     .finish();
//! // ureq:    agent.post(url).header("Content-Type", &content_type).send(&body)
//! // reqwest: client.post(url).header("Content-Type", content_type).body(body).send()
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Multipart {
    boundary: String,
    body: Vec<u8>,
}

impl Default for Multipart {
    fn default() -> Self {
        Self::new()
    }
}

impl Multipart {
    pub fn new() -> Multipart {
        static N: AtomicU64 = AtomicU64::new(0);
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        let boundary = format!("----trunk-recorder-{t:x}{:x}", N.fetch_add(1, Ordering::Relaxed));
        Multipart { boundary, body: Vec::new() }
    }

    /// A text field.
    pub fn text(mut self, name: &str, value: impl AsRef<str>) -> Multipart {
        self.part(&format!("Content-Disposition: form-data; name=\"{}\"", quote(name)), value.as_ref().as_bytes());
        self
    }

    /// A file field.
    pub fn file(mut self, name: &str, file_name: &str, content_type: &str, bytes: &[u8]) -> Multipart {
        let head = format!("Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {content_type}", quote(name), quote(file_name));
        self.part(&head, bytes);
        self
    }

    /// (body, the request's Content-Type)
    pub fn finish(mut self) -> (Vec<u8>, String) {
        self.body.extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        (self.body, format!("multipart/form-data; boundary={}", self.boundary))
    }

    fn part(&mut self, head: &str, bytes: &[u8]) {
        self.body.extend_from_slice(format!("--{}\r\n{head}\r\n\r\n", self.boundary).as_bytes());
        self.body.extend_from_slice(bytes);
        self.body.extend_from_slice(b"\r\n");
    }
}

fn quote(s: &str) -> String {
    s.replace('"', "%22").replace(['\r', '\n'], " ")
}

#[cfg(all(test, feature = "sdk"))]
mod tests {
    use super::*;
    use crate::testing::Request;

    #[test]
    fn reads_back() {
        let (body, ct) = Multipart::new().text("a", "1").file("call", "x.m4a", "audio/mp4", b"\0\x01bytes\r\n--").text("b", "two").finish();
        let r = Request { headers: vec![("content-type".into(), ct)], body, ..Default::default() };
        assert_eq!(r.form_field("a").unwrap(), b"1");
        assert_eq!(r.form_field("b").unwrap(), b"two");
        assert_eq!(r.form_field("call").unwrap(), b"\0\x01bytes\r\n--");
        assert_eq!(r.form_file_name("call").unwrap(), "x.m4a");
    }
}
