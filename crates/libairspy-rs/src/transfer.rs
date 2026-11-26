//! Vendor-request plumbing — the typed control-transfer layer every
//! device feature builds on, mirroring the `libusb_control_transfer`
//! call pattern used throughout `airspy.c`.
//!
//! Each helper sends one vendor request (`commands::Command` byte as
//! `bRequest`) with caller-supplied `wValue`/`wIndex` packing and
//! returns the transferred byte count; call sites apply the same
//! result checks the C library does (`!= 0` for empty writes,
//! `< length` for data transfers).

use core::time::Duration;

use crate::commands::Command;
use crate::device::Device;
use crate::error::Result;

/// `LIBUSB_CTRL_TIMEOUT_MS` (500) in airspy.c.
pub(crate) const CTRL_TIMEOUT: Duration = Duration::from_millis(500);

/// `LIBUSB_ENDPOINT_OUT | LIBUSB_REQUEST_TYPE_VENDOR |
/// LIBUSB_RECIPIENT_DEVICE` — the bmRequestType of every host-to-device
/// request in airspy.c (0x00 | 0x40 | 0x00) — see e.g.
/// `airspy_si5351c_write`'s `libusb_control_transfer` call.
pub(crate) const VENDOR_OUT_REQUEST_TYPE: u8 = 0x40;

/// Zero `wValue` for requests that carry no value parameter — the
/// literal `0` wValue argument in e.g. `airspy_set_freq`'s
/// `libusb_control_transfer` call (airspy.c).
pub(crate) const NO_WVALUE: u16 = 0;
/// Zero `wIndex` for requests that carry no index parameter — the
/// literal `0` wIndex argument in the same C calls.
pub(crate) const NO_WINDEX: u16 = 0;

/// `LIBUSB_ENDPOINT_IN | LIBUSB_REQUEST_TYPE_VENDOR |
/// LIBUSB_RECIPIENT_DEVICE` — the bmRequestType of every device-to-host
/// request in airspy.c (0x80 | 0x40 | 0x00).
pub(crate) const VENDOR_IN_REQUEST_TYPE: u8 = 0xC0;

impl Device {
    /// Host-to-device vendor request. Returns the number of bytes
    /// transferred; nusb transfer errors map to `Error::Transfer` (C's
    /// `AIRSPY_ERROR_LIBUSB`).
    pub(crate) fn vendor_out(
        &self,
        command: Command,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<usize> {
        self.usb_handle().write_control(
            VENDOR_OUT_REQUEST_TYPE,
            command as u8,
            value,
            index,
            data,
            CTRL_TIMEOUT,
        )
    }

    /// Device-to-host vendor request. Returns the number of bytes
    /// transferred into `data`.
    pub(crate) fn vendor_in(
        &self,
        command: Command,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize> {
        self.usb_handle().read_control(
            VENDOR_IN_REQUEST_TYPE,
            command as u8,
            value,
            index,
            data,
            CTRL_TIMEOUT,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_types_match_c_libusb_flags() {
        // Every control transfer in airspy.c uses one of these two
        // bmRequestType bytes: OUT|VENDOR|DEVICE (0x00|0x40|0x00) and
        // IN|VENDOR|DEVICE (0x80|0x40|0x00). The production transport
        // maps them to nusb's ControlType::Vendor + Recipient::Device
        // (direction implied by control_out vs control_in).
        assert_eq!(VENDOR_OUT_REQUEST_TYPE, 0x40);
        assert_eq!(VENDOR_IN_REQUEST_TYPE, 0xC0);
    }

    #[test]
    fn control_timeout_matches_c() {
        // LIBUSB_CTRL_TIMEOUT_MS (500) in airspy.c.
        assert_eq!(CTRL_TIMEOUT, core::time::Duration::from_millis(500));
    }
}
