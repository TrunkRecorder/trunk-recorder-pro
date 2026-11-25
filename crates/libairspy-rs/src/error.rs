//! Error type mirroring `enum airspy_error` from `airspy.h`.
//!
//! The C enum's two success values (`AIRSPY_SUCCESS`, `AIRSPY_TRUE`)
//! have no counterpart here — success is `Ok` in Rust. Every error
//! variant keeps its C integer code ([`Error::code`]) and its
//! `airspy_error_name()` string ([`Error::name`]) so tool output stays
//! diffable against the C tools.

/// Convenience alias used across the crate.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors reported by the driver, mirroring `enum airspy_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// `AIRSPY_ERROR_INVALID_PARAM` (-2)
    #[error("AIRSPY_ERROR_INVALID_PARAM (-2): invalid parameter")]
    InvalidParam,
    /// `AIRSPY_ERROR_NOT_FOUND` (-5)
    #[error("AIRSPY_ERROR_NOT_FOUND (-5): device not found")]
    NotFound,
    /// `AIRSPY_ERROR_BUSY` (-6)
    #[error("AIRSPY_ERROR_BUSY (-6): device busy")]
    Busy,
    /// `AIRSPY_ERROR_NO_MEM` (-11)
    #[error("AIRSPY_ERROR_NO_MEM (-11): out of memory")]
    NoMem,
    /// `AIRSPY_ERROR_UNSUPPORTED` (-12)
    #[error("AIRSPY_ERROR_UNSUPPORTED (-12): operation unsupported")]
    Unsupported,
    /// `AIRSPY_ERROR_LIBUSB` (-1000), carrying an underlying `nusb`
    /// device/enumeration error (open, claim, configuration). Clear-halt
    /// goes through `Interface::control_out`, so its failures surface as
    /// [`Error::Transfer`], not this variant.
    #[error("AIRSPY_ERROR_LIBUSB (-1000): {0}")]
    Usb(#[from] nusb::Error),
    /// `AIRSPY_ERROR_LIBUSB` (-1000), carrying an underlying `nusb`
    /// transfer error from a **control** transfer — the caller-visible
    /// path. Bulk-completion errors are handled inside the streaming worker
    /// (dropped and resubmitted, or, when unrecoverable, they stop the
    /// stream) and are never returned here. C folds every libusb failure
    /// into `AIRSPY_ERROR_LIBUSB`; both USB variants keep that code/name
    /// for reporting parity.
    #[error("AIRSPY_ERROR_LIBUSB (-1000): {0}")]
    Transfer(#[from] nusb::transfer::TransferError),
    /// A control transfer moved a different byte count than the
    /// request required — usually short, but C's `result != 0` check
    /// on empty requests also lands here with `actual > expected`.
    /// C folds all of these into `AIRSPY_ERROR_LIBUSB`; the port keeps
    /// that code/name for reporting parity while preserving the byte
    /// counts (more-correct deviation).
    #[error(
        "AIRSPY_ERROR_LIBUSB (-1000): transfer length mismatch, {actual} bytes (expected {expected})"
    )]
    TransferLengthMismatch {
        /// Bytes the request required.
        expected: usize,
        /// Bytes actually transferred.
        actual: usize,
    },
    /// `AIRSPY_ERROR_THREAD` (-1001)
    #[error("AIRSPY_ERROR_THREAD (-1001): worker thread failure")]
    Thread,
    /// `AIRSPY_ERROR_STREAMING_THREAD_ERR` (-1002)
    #[error("AIRSPY_ERROR_STREAMING_THREAD_ERR (-1002): streaming thread failure")]
    StreamingThread,
    /// `AIRSPY_ERROR_STREAMING_STOPPED` (-1003)
    #[error("AIRSPY_ERROR_STREAMING_STOPPED (-1003): streaming stopped")]
    StreamingStopped,
    /// `AIRSPY_ERROR_OTHER` (-9999)
    #[error("AIRSPY_ERROR_OTHER (-9999): unspecified error")]
    Other,
}

impl Error {
    /// The C `enum airspy_error` integer code for this error.
    #[must_use]
    pub const fn code(&self) -> i32 {
        match self {
            Self::InvalidParam => -2,
            Self::NotFound => -5,
            Self::Busy => -6,
            Self::NoMem => -11,
            Self::Unsupported => -12,
            Self::Usb(_) | Self::Transfer(_) | Self::TransferLengthMismatch { .. } => -1000,
            Self::Thread => -1001,
            Self::StreamingThread => -1002,
            Self::StreamingStopped => -1003,
            Self::Other => -9999,
        }
    }

    /// The constant name, exactly as `airspy_error_name()` returns it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::InvalidParam => "AIRSPY_ERROR_INVALID_PARAM",
            Self::NotFound => "AIRSPY_ERROR_NOT_FOUND",
            Self::Busy => "AIRSPY_ERROR_BUSY",
            Self::NoMem => "AIRSPY_ERROR_NO_MEM",
            // AIRSPY_ERROR_UNSUPPORTED is missing from the C switch in
            // airspy_error_name(), so C returns its default string; we
            // replicate that for output parity with the C tools.
            Self::Unsupported => "airspy unknown error",
            Self::Usb(_) | Self::Transfer(_) | Self::TransferLengthMismatch { .. } => {
                "AIRSPY_ERROR_LIBUSB"
            }
            Self::Thread => "AIRSPY_ERROR_THREAD",
            Self::StreamingThread => "AIRSPY_ERROR_STREAMING_THREAD_ERR",
            Self::StreamingStopped => "AIRSPY_ERROR_STREAMING_STOPPED",
            Self::Other => "AIRSPY_ERROR_OTHER",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_c_enum_airspy_error() {
        // Wire/ABI compatibility with `enum airspy_error` in airspy.h.
        assert_eq!(Error::InvalidParam.code(), -2);
        assert_eq!(Error::NotFound.code(), -5);
        assert_eq!(Error::Busy.code(), -6);
        assert_eq!(Error::NoMem.code(), -11);
        assert_eq!(Error::Unsupported.code(), -12);
        assert_eq!(
            Error::Transfer(nusb::transfer::TransferError::Fault).code(),
            -1000
        );
        assert_eq!(Error::Thread.code(), -1001);
        assert_eq!(Error::StreamingThread.code(), -1002);
        assert_eq!(Error::StreamingStopped.code(), -1003);
        assert_eq!(Error::Other.code(), -9999);
    }

    #[test]
    fn error_names_match_c_airspy_error_name() {
        // Same strings airspy_error_name() returns in airspy.c, so tool
        // output stays diffable against the C tools.
        assert_eq!(Error::InvalidParam.name(), "AIRSPY_ERROR_INVALID_PARAM");
        assert_eq!(Error::NotFound.name(), "AIRSPY_ERROR_NOT_FOUND");
        assert_eq!(Error::Busy.name(), "AIRSPY_ERROR_BUSY");
        assert_eq!(Error::NoMem.name(), "AIRSPY_ERROR_NO_MEM");
        // AIRSPY_ERROR_UNSUPPORTED has no case in C's airspy_error_name()
        // switch — it falls through to the default branch. C-exact means
        // replicating that quirk.
        assert_eq!(Error::Unsupported.name(), "airspy unknown error");
        assert_eq!(
            Error::Transfer(nusb::transfer::TransferError::Fault).name(),
            "AIRSPY_ERROR_LIBUSB"
        );
        assert_eq!(Error::Thread.name(), "AIRSPY_ERROR_THREAD");
        assert_eq!(
            Error::StreamingThread.name(),
            "AIRSPY_ERROR_STREAMING_THREAD_ERR"
        );
        assert_eq!(
            Error::StreamingStopped.name(),
            "AIRSPY_ERROR_STREAMING_STOPPED"
        );
        assert_eq!(Error::Other.name(), "AIRSPY_ERROR_OTHER");
    }

    #[test]
    fn transfer_length_mismatch_reports_like_c_libusb_error() {
        // Rust-side precision, C-compatible reporting: a length
        // mismatch is AIRSPY_ERROR_LIBUSB (-1000) to anything reading
        // codes/names, while the variant carries the byte counts.
        let err = Error::TransferLengthMismatch {
            expected: 4,
            actual: 1,
        };
        assert_eq!(err.code(), -1000);
        assert_eq!(err.name(), "AIRSPY_ERROR_LIBUSB");
        let msg = err.to_string();
        assert!(msg.contains('4') && msg.contains('1'), "got: {msg}");
    }

    #[test]
    fn transfer_errors_convert_into_transfer_variant() {
        use nusb::transfer::TransferError;
        let err: Error = TransferError::Stall.into();
        assert!(matches!(err, Error::Transfer(TransferError::Stall)));
    }

    #[test]
    fn display_includes_name_and_code() {
        // C tools print e.g. "AIRSPY_ERROR_NOT_FOUND (-5)"; Display keeps
        // both pieces available in one string.
        let msg = Error::NotFound.to_string();
        assert!(msg.contains("AIRSPY_ERROR_NOT_FOUND"), "got: {msg}");
        assert!(msg.contains("-5"), "got: {msg}");
    }
}
