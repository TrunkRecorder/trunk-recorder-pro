//! Device enumeration and lifecycle, ported from `airspy.c`'s
//! `airspy_list_devices` / `airspy_open_device` / `airspy_open_exit`.
//!
//! `airspy_open`/`airspy_close` pairs become RAII: [`Device::open`]
//! claims the interface, and dropping the [`Device`] releases it and
//! closes the handle. The Android `airspy_open_fd` path is out of
//! scope (see the design spec).

use nusb::MaybeFuture as _;

use std::sync::Arc;

use crate::commands::{Command, SampleType};
use crate::error::{Error, Result};
use crate::filters::{HB_KERNEL_FLOAT, HB_KERNEL_INT16};
use crate::stream::StreamWorkers;
use crate::transport::{NusbTransport, UsbTransport};

/// USB vendor id (`airspy_usb_vid` in airspy.c).
pub const AIRSPY_USB_VID: u16 = 0x1d50;
/// USB product id (`airspy_usb_pid` in airspy.c).
pub const AIRSPY_USB_PID: u16 = 0x60a1;

/// `STR_PREFIX_SERIAL_AIRSPY_SIZE` in airspy.c — chars before the hex
/// digits ("AIRSPY SN:").
const SERIAL_PREFIX_LEN: usize = 10;
/// `SERIAL_AIRSPY_EXPECTED_SIZE` in airspy.c — total serial-descriptor
/// length.
const SERIAL_EXPECTED_LEN: usize = 26;
/// `SERIAL_NUMBER_UNUSED` in airspy.c — `airspy_open_sn` treats serial
/// 0 as "no filter" and opens the first device found.
const SERIAL_NUMBER_UNUSED: u64 = 0;

/// Timeout for the serial string-descriptor reads. libusb's
/// `libusb_get_string_descriptor_ascii` hardcodes 1000 ms; match it.
const DESCRIPTOR_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(1);

/// USB configuration selected on open (`libusb_set_configuration(dev_handle, 1)`
/// in `airspy_open_device`).
const USB_CONFIGURATION: u8 = 1;
/// USB interface claimed on open and released on close
/// (`libusb_claim_interface(dev_handle, 0)` in `airspy_open_device`,
/// `libusb_release_interface(usb_device, 0)` in `airspy_open_exit`).
const USB_INTERFACE: u8 = 0;

/// Map an `airspy_open_sn`-style serial argument to an optional filter:
/// `SERIAL_NUMBER_UNUSED` (0) means "open the first device".
const fn serial_filter(serial_number: u64) -> Option<u64> {
    if serial_number == SERIAL_NUMBER_UNUSED {
        None
    } else {
        Some(serial_number)
    }
}

/// Fallback rate table from `airspy_open_init` (airspy.c), used when
/// the firmware `AIRSPY_GET_SAMPLERATES` query fails.
const FALLBACK_SAMPLERATES: [u32; 2] = [10_000_000, 2_500_000];

/// `airspy_get_samplerates` (airspy.c): non-IQ sample types — RAW
/// included — see the firmware rates doubled
/// (`!SAMPLE_TYPE_IS_IQ(device->sample_type)` → `buffer[i] *= 2`).
/// The non-IQ rate multiplier in `airspy_get_samplerates`'s
/// `buffer[i] *= 2` (airspy.c).
const NON_IQ_RATE_FACTOR: u32 = 2;

fn rates_for_sample_type(rates: &[u32], sample_type: SampleType) -> Vec<u32> {
    let is_iq = matches!(sample_type, SampleType::Float32Iq | SampleType::Int16Iq);
    rates
        .iter()
        // wrapping_mul: C's uint32_t multiply wraps on overflow.
        .map(|&r| {
            if is_iq {
                r
            } else {
                r.wrapping_mul(NON_IQ_RATE_FACTOR)
            }
        })
        .collect()
}

/// Cap on the firmware-reported rate count: a control transfer's
/// wLength is a `u16`, so more than `u16::MAX / 4` words cannot
/// arrive in one read (`airspy_read_samplerates_from_fw` in airspy.c
/// passes `count * sizeof(uint32_t)` through that cast, truncating).
const MAX_SAMPLERATE_COUNT: u32 = (u16::MAX as u32) / 4;

/// Decode little-endian `u32` sample rates from the bytes actually
/// transferred, dropping any partial trailing word (C leaves the tail
/// of its buffer uninitialized on a short read; we simply omit it).
fn samplerates_from_le_bytes(raw: &[u8]) -> Vec<u32> {
    raw.chunks_exact(4)
        .filter_map(|chunk| chunk.try_into().ok().map(u32::from_le_bytes))
        .collect()
}

/// Parse an Airspy serial-number string descriptor into its `u64`
/// serial, mirroring the C library: the descriptor must be exactly
/// [`SERIAL_EXPECTED_LEN`] chars, and the tail after the prefix is fed
/// through `strtoull(_, _, 16)` — parsing stops at the first non-hex
/// character and fails only when no digit was consumed at all.
fn parse_serial(descriptor: &str) -> Option<u64> {
    if descriptor.len() < SERIAL_EXPECTED_LEN {
        return None;
    }
    // C reads through libusb_get_string_descriptor_ascii into a
    // 27-byte buffer, which writes at most 26 chars — an over-long
    // descriptor arrives truncated to exactly SERIAL_EXPECTED_LEN and
    // is accepted. nusb returns the full string, so truncate to match.
    // `get` refuses (rather than panicking) if a cut lands off a
    // character boundary; the descriptor is ASCII in practice.
    let descriptor = descriptor.get(..SERIAL_EXPECTED_LEN)?;
    // C skips SERIAL_PREFIX_LEN raw bytes without validating their
    // content.
    let tail = descriptor.get(SERIAL_PREFIX_LEN..)?;
    strtoull_16(tail)
}

/// The `strtoull(start, &end, 16)` contract the C serial parse relies
/// on: skip leading whitespace, accept one optional sign ('-' negates
/// with unsigned wraparound), accept an optional `0x`/`0X` prefix when
/// a hex digit follows it, then consume hex digits greedily. `None`
/// iff no digit was consumed (C's `start == end` reject path).
fn strtoull_16(s: &str) -> Option<u64> {
    // C isspace(): space, \t, \n, \v, \f, \r — one char wider than
    // Rust's is_ascii_whitespace(), which lacks vertical tab.
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0B', '\x0C', '\r']);
    let (negative, s) = match s.strip_prefix(['+', '-']) {
        Some(rest) => (s.starts_with('-'), rest),
        None => (false, s),
    };
    // The prefix is consumed only when a hex digit follows; bare "0x"
    // parses as the digit '0' with 'x' left unconsumed.
    let s = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_hexdigit()) => rest,
        _ => s,
    };
    let digits: &str = s
        .find(|c: char| !c.is_ascii_hexdigit())
        .map_or(s, |end| &s[..end]);
    if digits.is_empty() {
        return None;
    }
    // At most 16 hex digits fit in the 26-char descriptor, so the
    // parse cannot overflow u64 (strtoull would saturate; unreachable
    // at this input length).
    let value = u64::from_str_radix(digits, 16).ok()?;
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

/// Enumerate connected devices matching the Airspy VID/PID via nusb.
/// Centralizes the filter both `airspy_list_devices` and
/// `airspy_open_device` perform in C.
fn airspy_devices() -> Result<impl Iterator<Item = nusb::DeviceInfo>> {
    Ok(nusb::list_devices()
        .wait()?
        .filter(|info| info.vendor_id() == AIRSPY_USB_VID && info.product_id() == AIRSPY_USB_PID))
}

/// Read and parse an Airspy's serial the way the C library does
/// (`airspy_list_devices` / `airspy_open_sn`): open the device and read
/// its serial *string descriptor*, rather than trusting nusb's
/// enumeration-time [`DeviceInfo::serial_number`], which can be `None`
/// even for a present device — dropping a supported Airspy and making
/// `open_serial` return `NotFound` (issue #75, finding #1).
///
/// Returns `None` when the device has no serial descriptor index, the
/// descriptor read fails, or the string doesn't parse (mirroring C's
/// "skip and keep scanning" on each of those).
fn read_serial(device: &nusb::Device) -> Option<u64> {
    // C only reads when iSerialNumber > 0.
    let index = device.device_descriptor().serial_number_string_index()?;
    // libusb_get_string_descriptor_ascii reads the language-ID table and
    // uses the first entry; fall back to US English (the Airspy firmware's
    // language) if that query fails or is empty.
    let language_id = device
        .get_string_descriptor_supported_languages(DESCRIPTOR_TIMEOUT)
        .wait()
        .ok()
        .and_then(|mut langs| langs.next())
        .unwrap_or(nusb::descriptors::language_id::US_ENGLISH);
    let descriptor = device
        .get_string_descriptor(index, language_id, DESCRIPTOR_TIMEOUT)
        .wait()
        .ok()?;
    parse_serial(&descriptor)
}

/// List the serial numbers of all connected Airspy devices, mirroring
/// `airspy_list_devices` (devices whose serial cannot be read or
/// parsed are skipped, as in C).
///
/// Deviation: the serial the operating system read at enumeration is
/// used when it parses, so listing doesn't open (and so disturb, or
/// fail on) a device another program is streaming from; only a device
/// without one is opened to read the descriptor, as C does for all.
pub fn list_devices() -> Result<Vec<u64>> {
    Ok(airspy_devices()?
        .filter_map(|info| {
            enumerated_serial(&info).or_else(|| read_serial(&info.open().wait().ok()?))
        })
        .collect())
}

/// The serial from the enumeration metadata, when the operating system
/// has it and it parses as an Airspy's.
fn enumerated_serial(info: &nusb::DeviceInfo) -> Option<u64> {
    info.serial_number().and_then(parse_serial)
}

/// An open Airspy device with interface 0 claimed.
///
/// Dropping the device releases the interface and closes the USB
/// handle (`airspy_open_exit` semantics).
pub struct Device {
    handle: Arc<dyn UsbTransport>,
    supported_samplerates: Vec<u32>,
    workers: Option<StreamWorkers>,
    sample_type: SampleType,
    /// `device->packing_enabled` — toggled by the packing setter (M4).
    pub(crate) packing_enabled: bool,
    /// The half-band kernels `cnv_f` / `cnv_i` are built from: the stock
    /// ones until `set_conversion_filter_*` replaces them.
    pub(crate) kernel_float: Vec<f32>,
    pub(crate) kernel_int16: Vec<i16>,
}

impl core::fmt::Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The transport's Debug may print internal pointers; keep
        // the output stable and address-free.
        f.debug_struct("Device")
            .field("supported_samplerates", &self.supported_samplerates)
            .field(
                "streaming",
                &self.workers.as_ref().is_some_and(|w| w.shared.running()),
            )
            .finish_non_exhaustive()
    }
}

impl Device {
    /// Open the first Airspy found (`airspy_open`).
    pub fn open() -> Result<Self> {
        Self::open_impl(None)
    }

    /// Open the Airspy with the given serial number (`airspy_open_sn`).
    ///
    /// Serial `0` is `SERIAL_NUMBER_UNUSED` in the C library: it opens
    /// the first device found instead of matching a literal zero
    /// serial, and this port preserves that contract.
    pub fn open_serial(serial_number: u64) -> Result<Self> {
        Self::open_impl(serial_filter(serial_number))
    }

    fn open_impl(serial_number: Option<u64>) -> Result<Self> {
        // Deviation: C reports NOT_FOUND whatever went wrong. A device
        // that was there but wouldn't open or claim (no WinUSB driver
        // bound, another program has it) reports that error instead, so
        // the caller can say why.
        let mut refused = None;
        for info in airspy_devices()? {
            let enumerated = enumerated_serial(&info);
            // A device the operating system already read another serial
            // from isn't opened at all (see `list_devices`).
            if let (Some(wanted), Some(serial)) = (serial_number, enumerated)
                && serial != wanted
            {
                continue;
            }
            // C opens each candidate (to read its serial descriptor when
            // matching) and closes+keeps scanning on any open failure.
            let device = match info.open().wait() {
                Ok(device) => device,
                Err(e) => {
                    // Only a device known to be the one asked for (or
                    // any, unfiltered) explains a failure to open.
                    if serial_number.is_none() || enumerated.is_some() {
                        refused = Some(Error::Usb(e));
                    }
                    continue;
                }
            };
            if let Some(wanted) = serial_number
                && enumerated.is_none()
            {
                // Without an enumerated serial, match against the one
                // read from the opened device's string descriptor
                // (issue #75, finding #1). C requires iSerialNumber > 0
                // and the exact descriptor length; read_serial/
                // parse_serial fold those into their None path.
                match read_serial(&device) {
                    Some(serial) if serial == wanted => {}
                    _ => continue,
                }
            }
            match Self::configure(&device) {
                Ok(interface) => {
                    return Ok(Self::from_transport(Arc::new(NusbTransport::new(
                        interface,
                    ))));
                }
                // C closes the handle and keeps scanning on any
                // configuration/claim failure.
                Err(e) => refused = Some(e),
            }
        }
        Err(refused.unwrap_or(Error::NotFound))
    }

    /// The supported sample rates for the currently selected sample
    /// type (`airspy_get_samplerates`; the C length/count two-call
    /// protocol collapses into a Vec).
    ///
    /// Like C, every non-IQ type — including RAW — reports the cached
    /// firmware rates doubled (`!SAMPLE_TYPE_IS_IQ` in airspy.c).
    #[must_use]
    pub fn samplerates(&self) -> Vec<u32> {
        rates_for_sample_type(&self.supported_samplerates, self.sample_type)
    }

    /// `airspy_read_samplerates_from_fw`: a 4-byte count query
    /// (wIndex = 0) followed by a count-word read (wIndex = count).
    fn read_samplerates_from_fw(&self) -> Result<Vec<u32>> {
        let mut count_raw = [0u8; 4];
        let n = self.vendor_in(Command::GetSamplerates, 0, 0, &mut count_raw)?;
        if n < 1 {
            // C: result < 1 → AIRSPY_ERROR_OTHER.
            return Err(Error::Other);
        }
        let count = u32::from_le_bytes(count_raw).min(MAX_SAMPLERATE_COUNT);
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut raw = vec![0u8; count as usize * 4];
        // count ≤ MAX_SAMPLERATE_COUNT < u16::MAX, so the fallback is
        // unreachable — it exists only to keep this conversion
        // panic-free without an unwrap.
        let index = u16::try_from(count).unwrap_or(u16::MAX);
        let n = self.vendor_in(Command::GetSamplerates, 0, index, &mut raw)?;
        if n < 1 {
            return Err(Error::Other);
        }
        Ok(samplerates_from_le_bytes(&raw[..n]))
    }

    /// Complete construction over an already-configured transport:
    /// `airspy_open_init`'s post-open half — defaults, then the
    /// samplerate cache with its fixed fallback pair. Test builds use
    /// this directly with a mock transport.
    pub(crate) fn from_transport(handle: Arc<dyn UsbTransport>) -> Self {
        let mut device = Self {
            handle,
            supported_samplerates: Vec::new(),
            workers: None,
            // airspy_open_init: sample_type = AIRSPY_SAMPLE_FLOAT32_IQ,
            // packing_enabled = false.
            sample_type: SampleType::Float32Iq,
            packing_enabled: false,
            kernel_float: HB_KERNEL_FLOAT.to_vec(),
            kernel_int16: HB_KERNEL_INT16.to_vec(),
        };
        // airspy_open_init caches the firmware rate table at open,
        // falling back to a fixed pair when the query fails.
        device.supported_samplerates = device
            .read_samplerates_from_fw()
            .unwrap_or_else(|_| FALLBACK_SAMPLERATES.to_vec());
        // airspy_open_init then switches packing off, whatever the last
        // program left the firmware in, ignoring the result (firmware
        // before v1.0.0-rc7 has no such request).
        let _ = device.set_packing(false);
        device
    }

    /// Borrow the underlying USB transport; the vendor-request layer
    /// in `transfer.rs` builds on this.
    pub(crate) fn usb_handle(&self) -> &dyn UsbTransport {
        self.handle.as_ref()
    }

    /// Clone the shared transport for the reader thread.
    pub(crate) fn usb_handle_arc(&self) -> Arc<dyn UsbTransport> {
        Arc::clone(&self.handle)
    }

    /// Select the delivered sample format (`airspy_set_sample_type`).
    ///
    /// C assigns unconditionally and its consumer reads the field per
    /// buffer (racing any mid-stream change); this port latches the
    /// type when [`Device::start_rx`] runs, so changing it during an
    /// active stream is refused with [`Error::Busy`] rather than
    /// silently deferred.
    pub fn set_sample_type(&mut self, sample_type: SampleType) -> Result<()> {
        if self.is_streaming() {
            return Err(Error::Busy);
        }
        self.sample_type = sample_type;
        Ok(())
    }

    /// The currently selected sample format.
    #[must_use]
    pub fn sample_type(&self) -> SampleType {
        self.sample_type
    }

    /// The undoubled firmware rate table (the values
    /// `airspy_set_samplerate` matches literal rates against).
    pub(crate) fn raw_samplerates(&self) -> &[u32] {
        &self.supported_samplerates
    }

    /// Streaming worker accessors for `stream.rs`.
    pub(crate) fn stream_workers(&self) -> Option<&StreamWorkers> {
        self.workers.as_ref()
    }

    pub(crate) fn stream_workers_mut(&mut self) -> Option<&mut StreamWorkers> {
        self.workers.as_mut()
    }

    pub(crate) fn set_stream_workers(&mut self, workers: Option<StreamWorkers>) {
        self.workers = workers;
    }

    pub(crate) fn take_stream_workers(&mut self) -> Option<StreamWorkers> {
        self.workers.take()
    }

    /// Kernel-driver detach + `set_configuration(1)` +
    /// `claim_interface(0)`, exactly as `airspy_open_device` does.
    ///
    /// nusb's `detach_and_claim_interface` performs the Linux-only
    /// kernel-driver detach; the configuration is (re)set only when it
    /// isn't already active, mirroring rusb's `set_active_configuration`
    /// and avoiding a needless device reset.
    fn configure(device: &nusb::Device) -> Result<nusb::Interface> {
        let active = device
            .active_configuration()
            .map_or(0, |config| config.configuration_value());
        if active != USB_CONFIGURATION {
            device.set_configuration(USB_CONFIGURATION).wait()?;
        }
        Ok(device.detach_and_claim_interface(USB_INTERFACE).wait()?)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // airspy_close stops streaming before airspy_open_exit releases
        // interface 0. release_interface is a no-op for the nusb
        // transport — dropping the Arc'd Interface (with this Device)
        // releases the claim and closes the handle.
        let _ = self.stop_rx();
        let _ = self.handle.release_interface(USB_INTERFACE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_ids_match_c_constants() {
        // airspy_usb_vid / airspy_usb_pid in airspy.c.
        assert_eq!(AIRSPY_USB_VID, 0x1d50);
        assert_eq!(AIRSPY_USB_PID, 0x60a1);
    }

    #[test]
    fn parses_wellformed_serial_descriptor() {
        // SERIAL_AIRSPY_EXPECTED_SIZE (26) chars: a 10-char prefix
        // (STR_PREFIX_SERIAL_AIRSPY_SIZE) then 16 hex digits.
        assert_eq!(
            parse_serial("AIRSPY SN:0123456789ABCDEF"),
            Some(0x0123_4567_89AB_CDEF)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:00000000644064DC"),
            Some(0x6440_64DC)
        );
        // strtoull accepts lowercase hex too.
        assert_eq!(
            parse_serial("AIRSPY SN:00000000644064dc"),
            Some(0x6440_64DC)
        );
    }

    #[test]
    fn rejects_serial_shorter_than_expected() {
        // C checks serial_number_len == SERIAL_AIRSPY_EXPECTED_SIZE
        // after a read that cannot exceed it, so short reads reject.
        assert_eq!(parse_serial("AIRSPY SN:123"), None);
        assert_eq!(parse_serial(""), None);
    }

    #[test]
    fn truncates_overlong_serial_like_c_buffer() {
        // libusb_get_string_descriptor_ascii writes at most 26 chars
        // into C's buffer, so a longer descriptor is truncated to 26
        // and accepted; nusb returns the whole string and we must
        // truncate to match.
        assert_eq!(
            parse_serial("AIRSPY SN:0123456789ABCDEF0"),
            Some(0x0123_4567_89AB_CDEF)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:0123456789ABCDEF-EXTRA-JUNK"),
            Some(0x0123_4567_89AB_CDEF)
        );
    }

    #[test]
    fn rejects_serial_with_no_hex_digits() {
        // C rejects only when strtoull consumed nothing
        // (serial == 0 && start == end).
        assert_eq!(parse_serial("AIRSPY SN:ZZZZZZZZZZZZZZZZ"), None);
        // Whitespace alone consumes no digits either.
        assert_eq!(parse_serial("AIRSPY SN:                "), None);
        // A sign with no digits after it consumes nothing.
        assert_eq!(parse_serial("AIRSPY SN:-ZZZZZZZZZZZZZZZ"), None);
    }

    #[test]
    fn skips_leading_whitespace_like_strtoull() {
        assert_eq!(
            parse_serial("AIRSPY SN:  123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:\t 123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        // C isspace() includes vertical tab, which Rust's
        // is_ascii_whitespace() does not.
        assert_eq!(
            parse_serial("AIRSPY SN:\x0B 123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
    }

    #[test]
    fn accepts_sign_like_strtoull() {
        assert_eq!(
            parse_serial("AIRSPY SN:+123456789ABCDEF"),
            Some(0x123_4567_89AB_CDEF)
        );
        // strtoull negates on '-' with unsigned wraparound.
        assert_eq!(
            parse_serial("AIRSPY SN:-1ZZZZZZZZZZZZZZ"),
            Some(1u64.wrapping_neg())
        );
    }

    #[test]
    fn accepts_hex_prefix_like_strtoull() {
        assert_eq!(
            parse_serial("AIRSPY SN:0x123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        assert_eq!(
            parse_serial("AIRSPY SN:0X123456789ABCDE"),
            Some(0x12_3456_789A_BCDE)
        );
        // "0x" not followed by a hex digit: strtoull consumes just the
        // "0" and stops, yielding 0 with digits consumed (accepted).
        assert_eq!(parse_serial("AIRSPY SN:0xZZZZZZZZZZZZZZ"), Some(0));
    }

    #[test]
    fn accepts_partial_hex_like_strtoull() {
        // Faithful strtoull semantics: parsing stops at the first
        // non-hex character, and the value parsed so far is accepted
        // as long as at least one digit was consumed.
        assert_eq!(parse_serial("AIRSPY SN:12ZZZZZZZZZZZZZZ"), Some(0x12));
    }

    #[test]
    fn zero_serial_parses_as_zero() {
        // All-zero digits: start != end, so C accepts serial 0.
        assert_eq!(parse_serial("AIRSPY SN:0000000000000000"), Some(0));
    }

    #[test]
    fn does_not_panic_on_multibyte_descriptor() {
        // The 10-byte skip must not panic when byte 10 falls inside a
        // multibyte character; C operates on raw bytes, we return None.
        let s = "AIRSPY SNé0123456789ABCDE"; // 'é' spans bytes 9–10; 26 bytes total
        assert_eq!(s.len(), SERIAL_EXPECTED_LEN);
        assert_eq!(parse_serial(s), None);
    }

    #[test]
    fn zero_serial_is_the_unused_sentinel() {
        // SERIAL_NUMBER_UNUSED (0ULL) in airspy.c: airspy_open_sn(0)
        // opens the first device instead of matching serial 0.
        assert_eq!(serial_filter(0), None);
        assert_eq!(serial_filter(0x6440_64DC), Some(0x6440_64DC));
    }

    // The tests below exercise the real USB stack and are written to
    // hold in any environment — CI runners with no USB devices AND a
    // dev box with a real Airspy attached.

    #[test]
    fn list_devices_enumerates_without_error() {
        // Zero devices in CI; each attached Airspy contributes a
        // parsed serial. Every u64 is a valid serial (C appends
        // whatever strtoull parsed), so success is the only invariant.
        let _serials = list_devices().expect("USB enumeration should succeed");
    }

    #[test]
    fn open_returns_device_or_not_found() {
        // Success and NotFound are both valid depending on what is
        // attached; open and enumeration are deliberately not equated —
        // open() reads no serial when unfiltered, and a listed device
        // can still fail configuration (both mirror C behavior). Any
        // other error means the scan loop broke.
        match Device::open() {
            // Usb: a device is attached and something else has it.
            Ok(_) | Err(crate::Error::NotFound | crate::Error::Usb(_)) => {}
            Err(other) => unreachable!("unexpected error: {other}"),
        }
    }

    #[test]
    fn open_with_unknown_serial_reports_not_found() {
        // Pick a nonzero serial provably absent from the attached
        // device set, so the test holds even with hardware plugged in.
        let serials = list_devices().expect("USB enumeration should succeed");
        let unknown_serial = (1..=u64::MAX)
            .find(|serial| !serials.contains(serial))
            .expect("a finite device list cannot contain every nonzero u64");
        assert!(matches!(
            Device::open_serial(unknown_serial),
            Err(crate::Error::NotFound)
        ));
    }

    #[test]
    fn fallback_table_matches_c() {
        // airspy_open_init's fallback when the firmware rate query
        // fails: {10000000, 2500000}.
        assert_eq!(FALLBACK_SAMPLERATES, [10_000_000, 2_500_000]);
    }

    #[test]
    fn non_iq_types_double_the_rates_like_c() {
        use crate::commands::SampleType;
        let rates = [10_000_000, 2_500_000];
        assert_eq!(
            rates_for_sample_type(&rates, SampleType::Float32Iq),
            vec![10_000_000, 2_500_000]
        );
        assert_eq!(
            rates_for_sample_type(&rates, SampleType::Int16Iq),
            vec![10_000_000, 2_500_000]
        );
        for t in [
            SampleType::Float32Real,
            SampleType::Int16Real,
            SampleType::Uint16Real,
            SampleType::Raw,
        ] {
            assert_eq!(
                rates_for_sample_type(&rates, t),
                vec![20_000_000, 5_000_000],
                "non-IQ type {t:?} must double"
            );
        }
    }

    #[test]
    fn samplerate_words_parse_little_endian() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&10_000_000u32.to_le_bytes());
        raw.extend_from_slice(&2_500_000u32.to_le_bytes());
        assert_eq!(samplerates_from_le_bytes(&raw), vec![10_000_000, 2_500_000]);
    }

    #[test]
    fn partial_trailing_word_is_dropped() {
        // Only complete words actually transferred are surfaced; C
        // would leave the tail of its malloc'd buffer uninitialized.
        let mut raw = Vec::new();
        raw.extend_from_slice(&6_000_000u32.to_le_bytes());
        raw.extend_from_slice(&[0x01, 0x02]);
        assert_eq!(samplerates_from_le_bytes(&raw), vec![6_000_000]);
    }
}
