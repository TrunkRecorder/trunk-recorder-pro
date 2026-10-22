//! Errors returned by the RTL-SDR driver.

use core::fmt;

/// Error returned by a RTL-SDR operation.
///
/// The variants preserve structured driver errors and the original `nusb` error values. Use
/// [`Error::kind`] when only a broad, backend-independent category is needed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A receiver configuration value failed validation.
    InvalidConfig {
        /// Name of the invalid configuration field.
        field: &'static str,
        /// Reason the value is invalid.
        reason: &'static str,
    },
    /// No matching RTL-SDR device was found.
    DeviceNotFound,
    /// The connected tuner is not an R820T/R828D-family device.
    UnsupportedTuner,
    /// The tuner PLL failed to lock after a retry.
    PllUnlocked,
    /// Configuration failed or was canceled; reapply a complete configuration.
    ConfigurationUnknown,
    /// The logical device session has been shut down.
    DeviceClosed,
    /// The device or USB resource is already in use.
    Busy,
    /// The requested operation is not supported by the backend or device.
    Unsupported,
    /// The stream is not running or is otherwise closed.
    StreamClosed {
        /// Reason the stream is no longer usable.
        reason: &'static str,
    },
    /// The USB backend returned an error outside an individual transfer.
    Usb(nusb::Error),
    /// An individual USB transfer failed.
    Transfer(nusb::transfer::TransferError),
    /// A RTL-SDR operation failed with a more specific source error.
    Operation {
        /// Operation being performed when the error occurred.
        operation: &'static str,
        /// More specific driver or USB error.
        source: Box<Error>,
    },
    /// The device or driver violated the expected RTL-SDR protocol.
    Protocol {
        /// Operation that failed.
        operation: &'static str,
        /// Protocol violation or unexpected response.
        reason: &'static str,
    },
}

/// Stable error category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A provided configuration value failed validation.
    InvalidConfig,
    /// No matching RTL-SDR device was found.
    NotFound,
    /// The logical device session has been shut down.
    DeviceClosed,
    /// The physical device was disconnected from USB.
    DeviceDisconnected,
    /// The device or USB resource is already in use.
    Busy,
    /// The requested operation is not supported by the backend or device.
    Unsupported,
    /// The USB backend returned an error.
    Usb,
    /// The stream is not running or is otherwise closed.
    StreamClosed,
    /// Any other driver or backend error.
    Other,
}

impl Error {
    /// Build a receiver configuration validation error.
    pub(crate) const fn invalid_config(field: &'static str, reason: &'static str) -> Self {
        Self::InvalidConfig { field, reason }
    }

    /// Build a stream lifecycle error.
    pub(crate) const fn stream_closed(reason: &'static str) -> Self {
        Self::StreamClosed { reason }
    }

    /// Build a RTL-SDR protocol error.
    pub(crate) const fn protocol(operation: &'static str, reason: &'static str) -> Self {
        Self::Protocol { operation, reason }
    }

    /// Attach driver-operation context while preserving the source error.
    pub(crate) fn at(self, operation: &'static str) -> Self {
        Self::Operation {
            operation,
            source: Box::new(self),
        }
    }

    /// Return a broad, backend-independent error category.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidConfig { .. } => ErrorKind::InvalidConfig,
            Self::DeviceNotFound => ErrorKind::NotFound,
            Self::UnsupportedTuner => ErrorKind::Unsupported,
            Self::PllUnlocked | Self::ConfigurationUnknown => ErrorKind::Other,
            Self::DeviceClosed => ErrorKind::DeviceClosed,
            Self::Busy => ErrorKind::Busy,
            Self::Unsupported => ErrorKind::Unsupported,
            Self::StreamClosed { .. } => ErrorKind::StreamClosed,
            Self::Usb(err) => match err.kind() {
                nusb::ErrorKind::Disconnected => ErrorKind::DeviceDisconnected,
                nusb::ErrorKind::Busy => ErrorKind::Busy,
                nusb::ErrorKind::NotFound => ErrorKind::NotFound,
                nusb::ErrorKind::Unsupported => ErrorKind::Unsupported,
                _ => ErrorKind::Usb,
            },
            Self::Transfer(nusb::transfer::TransferError::Disconnected) => {
                ErrorKind::DeviceDisconnected
            }
            Self::Transfer(_) => ErrorKind::Usb,
            Self::Operation { source, .. } => source.kind(),
            Self::Protocol { .. } => ErrorKind::Other,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(f, "invalid configuration for {field}: {reason}")
            }
            Self::DeviceNotFound => f.write_str("no matching RTL-SDR device found"),
            Self::UnsupportedTuner => f.write_str("unsupported tuner (R820T/R828D required)"),
            Self::PllUnlocked => f.write_str("tuner PLL did not lock"),
            Self::ConfigurationUnknown => {
                f.write_str("configuration is unknown; reapply a complete configuration or reopen")
            }
            Self::DeviceClosed => f.write_str("RTL-SDR device is closed"),
            Self::Busy => f.write_str("RTL-SDR device or USB resource is busy"),
            Self::Unsupported => f.write_str("operation is unsupported"),
            Self::StreamClosed { reason } => write!(f, "stream closed: {reason}"),
            Self::Usb(err) => write!(f, "USB error: {err}"),
            Self::Transfer(err) => write!(f, "USB transfer error: {err}"),
            Self::Operation { operation, source } => write!(f, "{operation}: {source}"),
            Self::Protocol { operation, reason } => write!(f, "{operation}: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Usb(err) => Some(err),
            Self::Transfer(err) => Some(err),
            Self::Operation { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<nusb::Error> for Error {
    fn from(value: nusb::Error) -> Self {
        Self::Usb(value)
    }
}

impl From<nusb::transfer::TransferError> for Error {
    fn from(value: nusb::transfer::TransferError) -> Self {
        Self::Transfer(value)
    }
}

/// Crate result alias using [`Error`].
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{Error, ErrorKind};

    #[test]
    fn transfer_errors_preserve_their_source_and_are_not_configuration_errors() {
        let err = Error::from(nusb::transfer::TransferError::InvalidArgument);

        assert!(matches!(
            &err,
            Error::Transfer(nusb::transfer::TransferError::InvalidArgument)
        ));
        assert_eq!(err.kind(), ErrorKind::Usb);
        assert!(err.source().is_some());
    }

    #[test]
    fn transfer_disconnects_have_their_own_error_category() {
        let err = Error::from(nusb::transfer::TransferError::Disconnected);

        assert_eq!(err.kind(), ErrorKind::DeviceDisconnected);
        assert!(err.source().is_some());
    }

    #[test]
    fn configuration_errors_expose_the_field_and_reason() {
        let err = Error::invalid_config("frequency_hz", "must be nonzero");

        assert!(matches!(
            &err,
            Error::InvalidConfig {
                field: "frequency_hz",
                reason: "must be nonzero"
            }
        ));
        assert_eq!(err.kind(), ErrorKind::InvalidConfig);
    }

    #[test]
    fn operation_context_preserves_source_category() {
        let err =
            Error::from(nusb::transfer::TransferError::Fault).at("applying receiver configuration");

        assert_eq!(err.kind(), ErrorKind::Usb);
        assert!(
            err.to_string()
                .starts_with("applying receiver configuration: USB transfer error:")
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn operation_context_preserves_disconnect_category() {
        let err =
            Error::from(nusb::transfer::TransferError::Disconnected).at("reading receiver samples");

        assert_eq!(err.kind(), ErrorKind::DeviceDisconnected);
        assert!(err.source().is_some());
    }

    #[test]
    fn closed_device_has_its_own_error_category() {
        assert_eq!(Error::DeviceClosed.kind(), ErrorKind::DeviceClosed);
        assert_eq!(Error::DeviceClosed.to_string(), "RTL-SDR device is closed");
    }
}
