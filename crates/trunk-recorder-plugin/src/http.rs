//! What an HTTP answer's status means for something a plugin sends (a call upload, a health
//! report), read the same way by every plugin: when to try again, when to wait as the service
//! asks (`Retry-After`), when to give up. [`crate::Attempt::from_status`] makes an upload's
//! outcome of it, [`crate::health::Reporter::failed`] takes it for a report.

use std::time::Duration;

/// Why something sent wasn't taken ([`Failure::from_status`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Failure {
    /// Not reached, timed out (`408`), or a server error (`5xx` but `503`): try again.
    Unreachable,
    /// `429` / `503`: busy, or paused. Wait this long if the service said (`Retry-After`), else back off.
    Busy(Option<Duration>),
    /// `401` / `403`: the credentials (an API key) were refused. Trying again won't help.
    Refused,
    /// `404` / `405` / `410`: nothing at that address takes it (a wrong server, or one that doesn't
    /// take this yet).
    NotThere,
    /// Any other `4xx`: what was sent was wrong. The same again won't do better.
    Rejected,
}

impl Failure {
    /// What answer `status` means (none: `2xx`, taken), with its `Retry-After` header if any.
    pub fn from_status(status: u16, retry_after: Option<&str>) -> Option<Failure> {
        Some(match status {
            200..=299 => return None,
            401 | 403 => Failure::Refused,
            404 | 405 | 410 => Failure::NotThere,
            429 | 503 => Failure::Busy(retry_after.and_then(parse_retry_after)),
            408 => Failure::Unreachable,
            400..=499 => Failure::Rejected,
            _ => Failure::Unreachable,
        })
    }

    /// Worth trying the same again (later).
    pub fn retry(&self) -> bool {
        matches!(self, Failure::Unreachable | Failure::Busy(_))
    }
}

/// A `Retry-After` header's wait: seconds (`120`). (A date isn't read: none.)
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let s = value.trim().parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0)?;
    Some(Duration::from_secs_f64(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_says_whether_to_try_again() {
        let of = |s: u16| Failure::from_status(s, None);
        assert_eq!((of(200), of(204)), (None, None));
        assert_eq!((of(401), of(403)), (Some(Failure::Refused), Some(Failure::Refused)));
        assert_eq!((of(404), of(405), of(410)), (Some(Failure::NotThere), Some(Failure::NotThere), Some(Failure::NotThere)));
        assert_eq!((of(400), of(413), of(417)), (Some(Failure::Rejected), Some(Failure::Rejected), Some(Failure::Rejected)));
        assert_eq!((of(408), of(500), of(502), of(0)), (Some(Failure::Unreachable), Some(Failure::Unreachable), Some(Failure::Unreachable), Some(Failure::Unreachable)));
        assert_eq!(Failure::from_status(503, Some(" 120 ")), Some(Failure::Busy(Some(Duration::from_secs(120)))));
        assert_eq!(Failure::from_status(429, Some("Wed, 21 Oct 2026 07:28:00 GMT")), Some(Failure::Busy(None)));
        assert_eq!(Failure::from_status(429, Some("-5")), Some(Failure::Busy(None)));
        assert!(of(500).unwrap().retry() && of(429).unwrap().retry() && !of(401).unwrap().retry() && !of(404).unwrap().retry());
    }
}
