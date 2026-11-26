//! Device control-surface setters, ported from `airspy.c`.

use crate::commands::{Command, GpioPin, GpioPort, SampleType};
use crate::device::{Device, Family};
use crate::error::{Error, Result};
use crate::stream::BULK_ENDPOINT;
use crate::transfer::{NO_WINDEX, NO_WVALUE};

/// `MIN_SAMPLERATE_BY_VALUE` (airspy.c) — arguments at or above this
/// are literal rates in Hz; below it they are table indices.
const MIN_SAMPLERATE_BY_VALUE: u32 = 1_000_000;

/// `GAIN_COUNT` (airspy.c) — entries in each composite-gain table.
const GAIN_COUNT: usize = 22;

// The composite-gain tables from airspy.c (BSD-3-Clause), transcribed
// verbatim: airspy_linearity_*_gains / airspy_sensitivity_*_gains.
#[rustfmt::skip]
const LINEARITY_VGA_GAINS: [u8; GAIN_COUNT] =
    [13, 12, 11, 11, 11, 11, 11, 10, 10, 10, 10, 10, 10, 10, 10, 10, 9, 8, 7, 6, 5, 4];
#[rustfmt::skip]
const LINEARITY_MIXER_GAINS: [u8; GAIN_COUNT] =
    [12, 12, 11, 9, 8, 7, 6, 6, 5, 0, 0, 1, 0, 0, 2, 2, 1, 1, 1, 1, 0, 0];
#[rustfmt::skip]
const LINEARITY_LNA_GAINS: [u8; GAIN_COUNT] =
    [14, 14, 14, 13, 12, 10, 9, 9, 8, 9, 8, 6, 5, 3, 1, 0, 0, 0, 0, 0, 0, 0];
#[rustfmt::skip]
const SENSITIVITY_VGA_GAINS: [u8; GAIN_COUNT] =
    [13, 12, 11, 10, 9, 8, 7, 6, 5, 5, 5, 5, 5, 4, 4, 4, 4, 4, 4, 4, 4, 4];
#[rustfmt::skip]
const SENSITIVITY_MIXER_GAINS: [u8; GAIN_COUNT] =
    [12, 12, 12, 12, 11, 10, 10, 9, 9, 8, 7, 4, 4, 4, 3, 2, 2, 1, 0, 0, 0, 0];
#[rustfmt::skip]
const SENSITIVITY_LNA_GAINS: [u8; GAIN_COUNT] =
    [14, 14, 14, 14, 14, 14, 14, 14, 14, 13, 12, 12, 9, 9, 8, 7, 6, 5, 3, 2, 1, 0];

/// The shortest half-band kernel the converters take: a centre tap and
/// one tap either side.
pub const MIN_KERNEL_LEN: usize = 3;

/// `airspy_set_lna_gain`'s clamp bound.
const LNA_GAIN_MAX: u8 = 14;
/// `airspy_set_mixer_gain` / `airspy_set_vga_gain`'s clamp bound.
const MIXER_VGA_GAIN_MAX: u8 = 15;

impl Device {
    /// Tune the receiver (`airspy_set_freq`): the target frequency in
    /// Hz travels as a 4-byte little-endian payload (`set_freq_params_t`
    /// with `TO_LE`, airspy.c).
    ///
    /// The C header documents 24 MHz – 1.75 GHz as the valid range but
    /// the library does not enforce it, and neither does this port —
    /// the firmware is the authority.
    ///
    /// An RFOne takes it as 8 bytes (`hydrasdr_generic_set_freq`'s
    /// `uint64_t`, hydrasdr_shared.c).
    pub fn set_freq(&self, freq_hz: u32) -> Result<()> {
        match self.family() {
            Family::Airspy => self.out_setter(
                Command::SetFreq,
                NO_WVALUE,
                NO_WINDEX,
                &freq_hz.to_le_bytes(),
            ),
            Family::Rfone => self.out_setter(
                Command::SetFreq,
                NO_WVALUE,
                NO_WINDEX,
                &u64::from(freq_hz).to_le_bytes(),
            ),
        }
    }
}

impl Device {
    /// The OUT pattern shared by `set_freq` and the GPIO writes: send
    /// the payload and require the exact transferred length (C's
    /// `result != length` checks).
    pub(crate) fn out_setter(
        &self,
        command: Command,
        value: u16,
        index: u16,
        payload: &[u8],
    ) -> Result<()> {
        let n = self.vendor_out(command, value, index, payload)?;
        if n != payload.len() {
            return Err(Error::TransferLengthMismatch {
                expected: payload.len(),
                actual: n,
            });
        }
        Ok(())
    }

    /// The IN-with-status pattern most C setters share: a 1-byte read
    /// with the value in `wIndex`, `result < 1` mapping to the libusb
    /// error.
    fn in_setter(&self, command: Command, index: u16) -> Result<()> {
        let mut status = [0u8; 1];
        let n = self.vendor_in(command, NO_WVALUE, index, &mut status)?;
        if n < status.len() {
            return Err(Error::TransferLengthMismatch {
                expected: status.len(),
                actual: n,
            });
        }
        Ok(())
    }

    /// Select the sample rate (`airspy_set_samplerate`): the argument
    /// is either a table index or a literal rate in Hz (at or above
    /// `MIN_SAMPLERATE_BY_VALUE`).
    ///
    /// Like C, literal rates match against the **undoubled firmware
    /// table** — for non-IQ sample types [`Device::samplerates`]
    /// reports doubled values, and passing one of those doubled rates
    /// takes the off-table kHz path rather than matching an index
    /// (faithful to `airspy_set_samplerate`). Off-table rates are
    /// doubled for IQ types and sent in kHz, exactly as C computes
    /// them.
    pub fn set_samplerate(&self, samplerate: u32) -> Result<()> {
        let mut value = samplerate;
        if value >= MIN_SAMPLERATE_BY_VALUE {
            if let Some(i) = self.raw_samplerates().iter().position(|&r| r == value) {
                value = u32::try_from(i).unwrap_or(u32::MAX);
            } else {
                if matches!(
                    self.sample_type(),
                    SampleType::Float32Iq | SampleType::Int16Iq
                ) {
                    value = value.wrapping_mul(2);
                }
                value /= 1000;
            }
        }
        // C clears the bulk endpoint halt before the request and ignores
        // the result. nusb's Endpoint::clear_halt claims the endpoint and
        // must NOT run while transfers are in flight on it, so we skip the
        // clear while streaming: a running stream is not halted, and
        // resetting its host data toggle mid-flight would corrupt it
        // (issue #75, finding #2). The common case — set the rate before
        // start_rx — is unaffected. Like C, the clear's result is ignored.
        if !self.is_streaming() {
            let _ = self.usb_handle().clear_halt(BULK_ENDPOINT);
        }
        // C passes the u32 into libusb's u16 wIndex — same truncation.
        #[allow(clippy::cast_possible_truncation)]
        self.in_setter(Command::SetSamplerate, value as u16)
    }

    /// `airspy_set_lna_gain` (0–14; larger values clamp, like C).
    pub fn set_lna_gain(&self, value: u8) -> Result<()> {
        self.in_setter(Command::SetLnaGain, u16::from(value.min(LNA_GAIN_MAX)))
    }

    /// `airspy_set_mixer_gain` (0–15; larger values clamp, like C).
    pub fn set_mixer_gain(&self, value: u8) -> Result<()> {
        self.in_setter(
            Command::SetMixerGain,
            u16::from(value.min(MIXER_VGA_GAIN_MAX)),
        )
    }

    /// `airspy_set_vga_gain` (0–15; larger values clamp, like C).
    pub fn set_vga_gain(&self, value: u8) -> Result<()> {
        self.in_setter(
            Command::SetVgaGain,
            u16::from(value.min(MIXER_VGA_GAIN_MAX)),
        )
    }

    /// `airspy_set_lna_agc`.
    pub fn set_lna_agc(&self, enabled: bool) -> Result<()> {
        self.in_setter(Command::SetLnaAgc, u16::from(enabled))
    }

    /// `airspy_set_mixer_agc`.
    pub fn set_mixer_agc(&self, enabled: bool) -> Result<()> {
        self.in_setter(Command::SetMixerAgc, u16::from(enabled))
    }

    /// The shared composite-gain sequence of `airspy_set_linearity_gain`
    /// and `airspy_set_sensitivity_gain`: clamp, reverse the index,
    /// disable both AGCs, then write the three table gains.
    fn set_composite_gain(
        &self,
        value: u8,
        vga: &[u8; GAIN_COUNT],
        mixer: &[u8; GAIN_COUNT],
        lna: &[u8; GAIN_COUNT],
    ) -> Result<()> {
        let clamped = usize::from(value).min(GAIN_COUNT - 1);
        let idx = GAIN_COUNT - 1 - clamped;
        self.set_mixer_agc(false)?;
        self.set_lna_agc(false)?;
        self.set_vga_gain(vga[idx])?;
        self.set_mixer_gain(mixer[idx])?;
        self.set_lna_gain(lna[idx])
    }

    /// `airspy_set_linearity_gain` (0–21).
    pub fn set_linearity_gain(&self, value: u8) -> Result<()> {
        self.set_composite_gain(
            value,
            &LINEARITY_VGA_GAINS,
            &LINEARITY_MIXER_GAINS,
            &LINEARITY_LNA_GAINS,
        )
    }

    /// `airspy_set_sensitivity_gain` (0–21).
    pub fn set_sensitivity_gain(&self, value: u8) -> Result<()> {
        self.set_composite_gain(
            value,
            &SENSITIVITY_VGA_GAINS,
            &SENSITIVITY_MIXER_GAINS,
            &SENSITIVITY_LNA_GAINS,
        )
    }

    /// Write a GPIO level (`airspy_gpio_write`): `wValue` carries the
    /// level (0 or 1), `wIndex` packs `port << 5 | pin`.
    pub fn gpio_write(&self, port: GpioPort, pin: GpioPin, value: u8) -> Result<()> {
        let port_pin = u16::from((port as u8) << 5 | pin as u8);
        self.out_setter(Command::GpioWrite, u16::from(value), port_pin, &[])
    }

    /// Toggle the bias tee (`airspy_set_rf_bias` — a GPIO write to
    /// port 1 pin 13). An RFOne's has a request of its own, the value
    /// in `wIndex` (`hydrasdr_generic_set_rf_bias`); it powers the
    /// first input alone.
    pub fn set_rf_bias(&self, enabled: bool) -> Result<()> {
        match self.family() {
            Family::Airspy => self.gpio_write(GpioPort::Port1, GpioPin::Pin13, u8::from(enabled)),
            Family::Rfone => {
                self.out_setter(Command::SetRfBias, NO_WVALUE, u16::from(enabled), &[])
            }
        }
    }

    /// Pick the input an RFOne hears, by its place in
    /// [`crate::device::RFONE_RF_PORTS`] (`hydrasdr_generic_set_rf_port`):
    /// the firmware answers 1 for an input it has, and anything else is
    /// [`Error::InvalidParam`]. It keeps the input after it is closed.
    /// An Airspy has the one: [`Error::Unsupported`].
    pub fn set_rf_port(&self, port: u8) -> Result<()> {
        if self.family() != Family::Rfone {
            return Err(Error::Unsupported);
        }
        let mut status = [0u8; 1];
        let n = self.vendor_in(Command::SetRfPort, NO_WVALUE, u16::from(port), &mut status)?;
        if n < status.len() {
            return Err(Error::TransferLengthMismatch {
                expected: status.len(),
                actual: n,
            });
        }
        if status[0] != 1 {
            return Err(Error::InvalidParam);
        }
        Ok(())
    }

    /// Toggle 12-bit packed transfers (`airspy_set_packing`). Refused
    /// with [`Error::Busy`] while streaming, like C.
    ///
    /// Deviation: C cancels and reallocates its transfer buffers here;
    /// this engine sizes its buffers per `start_rx`, so only the flag
    /// changes.
    pub fn set_packing(&mut self, enabled: bool) -> Result<()> {
        if self.is_streaming() {
            return Err(Error::Busy);
        }
        self.in_setter(Command::SetPacking, u16::from(enabled))?;
        self.packing_enabled = enabled;
        Ok(())
    }

    /// Replace the half-band kernel of the float IQ converter
    /// (`airspy_set_conversion_filter_float32`). Refused with
    /// [`Error::Busy`] while streaming, like C; the next `start_rx`
    /// builds its converter from it.
    ///
    /// Deviation: C takes any length (and reads past an even-length
    /// kernel's end); a half-band kernel has an odd number of taps, at
    /// least [`MIN_KERNEL_LEN`], and anything else is
    /// [`Error::InvalidParam`].
    pub fn set_conversion_filter_float32(&mut self, kernel: &[f32]) -> Result<()> {
        self.check_kernel(kernel.len())?;
        self.kernel_float = kernel.to_vec();
        Ok(())
    }

    /// Replace the half-band kernel of the int16 IQ converter
    /// (`airspy_set_conversion_filter_int16`); as
    /// [`Device::set_conversion_filter_float32`].
    pub fn set_conversion_filter_int16(&mut self, kernel: &[i16]) -> Result<()> {
        self.check_kernel(kernel.len())?;
        self.kernel_int16 = kernel.to_vec();
        Ok(())
    }

    fn check_kernel(&self, len: usize) -> Result<()> {
        if self.is_streaming() {
            return Err(Error::Busy);
        }
        if len < MIN_KERNEL_LEN || len % 2 == 0 {
            return Err(Error::InvalidParam);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use crate::device::{Device, Family};
    use crate::transport::mock::{MockTransport, wire};

    /// A Device over a mock transport, with construction-time traffic
    /// (the samplerate query) already drained.
    pub(crate) fn mock_device() -> (Arc<MockTransport>, Device) {
        mock_family(Family::Airspy)
    }

    /// [`mock_device`], for either family.
    pub(crate) fn mock_family(family: Family) -> (Arc<MockTransport>, Device) {
        let transport = Arc::new(MockTransport::default());
        // Unscripted reads fail, so the open-time samplerate query
        // takes the C fallback table deterministically.
        let device = Device::from_transport_as(family, Arc::clone(&transport) as Arc<_>);
        transport.take_recorded();
        (transport, device)
    }

    #[test]
    fn rfone_set_freq_is_eight_bytes() {
        // hydrasdr_generic_set_freq: bRequest HYDRASDR_SET_FREQ (13),
        // an 8-byte little-endian payload.
        let (transport, device) = mock_family(Family::Rfone);
        device.set_freq(100_000_000).expect("set_freq");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::SET_FREQ);
        assert_eq!((c.value, c.index), (0, 0));
        assert_eq!(c.data, vec![0x00, 0xE1, 0xF5, 0x05, 0, 0, 0, 0]);
    }

    #[test]
    fn rfone_bias_is_its_own_request() {
        // hydrasdr_generic_set_rf_bias: HYDRASDR_SET_RF_BIAS_CMD (20),
        // OUT, the value in wIndex, no payload.
        let (transport, device) = mock_family(Family::Rfone);
        device.set_rf_bias(true).expect("bias");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, 20);
        assert_eq!((c.value, c.index), (0, 1));
        assert!(c.data.is_empty());
    }

    #[test]
    fn rfone_input_is_what_the_firmware_takes() {
        // hydrasdr_generic_set_rf_port: HYDRASDR_SET_RF_PORT (28), IN,
        // the port in wIndex, one byte back that must be 1.
        let (transport, device) = mock_family(Family::Rfone);
        transport.script_reads(vec![Ok(vec![1]), Ok(vec![0])]);
        device.set_rf_port(2).expect("CABLE2");
        assert!(matches!(
            device.set_rf_port(7),
            Err(crate::Error::InvalidParam)
        ));
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 2);
        assert_in_setter(&calls[0], 28, 2);
        // An Airspy isn't asked.
        let (transport, device) = mock_device();
        assert!(matches!(
            device.set_rf_port(0),
            Err(crate::Error::Unsupported)
        ));
        assert!(transport.take_recorded().is_empty());
    }

    #[test]
    fn rfone_falls_back_to_its_own_rates() {
        let (_, device) = mock_family(Family::Rfone);
        assert_eq!(device.samplerates(), vec![10_000_000, 5_000_000, 2_500_000]);
    }

    #[test]
    fn freq_payload_is_little_endian() {
        // set_freq_params.freq_hz = TO_LE(freq_hz): 100 MHz on the
        // wire is 00 E1 F5 05.
        assert_eq!(100_000_000u32.to_le_bytes(), [0x00, 0xE1, 0xF5, 0x05]);
    }

    #[test]
    fn set_freq_wire_contract() {
        // The full airspy_set_freq transfer: bmRequestType 0x40,
        // bRequest AIRSPY_SET_FREQ (13), zero wValue/wIndex, 4-byte
        // little-endian payload, LIBUSB_CTRL_TIMEOUT_MS.
        let (transport, device) = mock_device();
        device.set_freq(100_000_000).expect("set_freq");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::SET_FREQ);
        assert_eq!((c.value, c.index), (0, 0));
        assert_eq!(c.data, vec![0x00, 0xE1, 0xF5, 0x05]);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    #[test]
    fn set_freq_short_write_maps_to_length_mismatch() {
        let (transport, device) = mock_device();
        transport.script_writes(vec![Ok(vec![0u8; 2])]); // 2 of 4 bytes
        let err = device.set_freq(1).expect_err("short write");
        assert!(matches!(
            err,
            crate::Error::TransferLengthMismatch {
                expected: 4,
                actual: 2
            }
        ));
    }

    #[test]
    fn set_freq_usb_error_passes_through() {
        let (transport, device) = mock_device();
        transport.script_writes(vec![Err(nusb::transfer::TransferError::Stall)]);
        let err = device.set_freq(1).expect_err("usb error");
        assert!(matches!(
            err,
            crate::Error::Transfer(nusb::transfer::TransferError::Stall)
        ));
    }

    #[test]
    fn board_id_read_wire_contract_and_value() {
        let (transport, device) = mock_device();
        // Non-zero id: proves response bytes actually reach the
        // caller (a zeroed buffer can't fake it).
        transport.script_reads(vec![Ok(vec![0x42u8])]);
        let id = device.board_id().expect("board id");
        assert_eq!(id, 0x42);
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_IN);
        assert_eq!(c.request, wire::BOARD_ID_READ);
        assert_eq!((c.value, c.index), (0, 0));
        assert_eq!(c.data.len(), 1);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    #[test]
    fn stop_rx_sends_receiver_off_even_without_stream() {
        // airspy_stop_rx sends RECEIVER_MODE_OFF unconditionally.
        let (transport, mut device) = mock_device();
        device.stop_rx().expect("stop");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::RECEIVER_MODE);
        assert_eq!(c.value, wire::RECEIVER_MODE_OFF);
        assert_eq!(c.index, 0);
        assert!(c.data.is_empty());
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    #[test]
    fn from_transport_caches_scripted_samplerates() {
        let transport = Arc::new(MockTransport::default());
        // Count query (4 bytes LE) then the rate list.
        transport.script_reads(vec![
            Ok(2u32.to_le_bytes().to_vec()),
            Ok([6_000_000u32, 3_000_000u32]
                .iter()
                .flat_map(|r| r.to_le_bytes())
                .collect()),
        ]);
        let device = Device::from_transport(transport as Arc<_>);
        // Default type is Float32Iq: rates undoubled.
        assert_eq!(device.samplerates(), vec![6_000_000, 3_000_000]);
    }

    #[test]
    fn from_transport_falls_back_when_query_fails() {
        // Construct directly so the construction-time recording is
        // retained: the fallback must be reached BECAUSE the firmware
        // query failed, not silently.
        let transport = Arc::new(MockTransport::default());
        let device = Device::from_transport(Arc::clone(&transport) as Arc<_>);
        let calls = transport.take_recorded();
        // Then packing off, as airspy_open_init leaves every device.
        assert_eq!(calls.len(), 2, "the count query, then packing off");
        assert_eq!(calls[0].request, wire::GET_SAMPLERATES);
        assert_in_setter(&calls[1], wire::SET_PACKING, 0);
        // The C fallback pair, undoubled for the IQ default; asserted
        // against the independent wire transcription so production
        // drift is caught.
        assert_eq!(device.samplerates(), wire::FALLBACK_SAMPLERATES.to_vec());
    }

    #[test]
    fn conversion_filters_take_a_half_band_kernel_when_idle() {
        use crate::transport::mock::BulkRead;
        let (transport, mut device) = mock_device();
        // No taps, too few, or an even count: not a half-band kernel.
        for bad in [0usize, 1, 2, 4, 46] {
            assert!(matches!(
                device.set_conversion_filter_float32(&vec![0.0; bad]),
                Err(crate::Error::InvalidParam)
            ));
            assert!(matches!(
                device.set_conversion_filter_int16(&vec![0; bad]),
                Err(crate::Error::InvalidParam)
            ));
        }
        // A stream started after a change converts with the new kernel:
        // a kernel of only a centre tap of 1 passes I through the FIR
        // as zeros and leaves Q scaled by 1, unlike the stock one.
        let kernel = [0i16, 32767, 0];
        device.set_conversion_filter_int16(&kernel).expect("int16");
        device
            .set_conversion_filter_float32(&[0.0, 1.0, 0.0])
            .expect("float32");
        assert_eq!(device.kernel_int16, kernel);
        assert_eq!(device.kernel_float, [0.0, 1.0, 0.0]);
        device
            .set_sample_type(crate::commands::SampleType::Int16Iq)
            .expect("type");
        transport.script_bulk(vec![BulkRead::Fill(0x08)]);
        device.start_rx_polled().expect("start");
        // Refused while streaming, like C.
        assert!(matches!(
            device.set_conversion_filter_float32(&[0.0, 1.0, 0.0]),
            Err(crate::Error::Busy)
        ));
        assert!(matches!(
            device.set_conversion_filter_int16(&kernel),
            Err(crate::Error::Busy)
        ));
        let i_all_zero = device
            .recv(core::time::Duration::from_secs(5), |t| {
                let crate::Samples::Int16(s) = t.samples else {
                    unreachable!("INT16_IQ stream");
                };
                s.iter().step_by(2).all(|&i| i == 0)
            })
            .expect("recv");
        assert_eq!(i_all_zero, Some(true), "the outer taps are zero");
        device.stop_rx().expect("stop");
    }

    /// The IN-with-status pattern most setters use: one `read_control`
    /// with the value in wIndex and a 1-byte status buffer.
    fn assert_in_setter(c: &crate::transport::mock::ControlCall, request: u8, index: u16) {
        assert_eq!(c.request_type, wire::VENDOR_IN);
        assert_eq!(c.request, request);
        assert_eq!((c.value, c.index), (0, index));
        assert_eq!(c.data.len(), 1);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
    }

    /// `airspy_set_samplerate` clears the bulk-endpoint halt (host-toggle
    /// reset, issue #75) BEFORE the `SET_SAMPLERATE` control transfer. The
    /// mock records that clear as a sentinel so its presence and ordering
    /// are observable.
    fn assert_clear_halt(c: &crate::transport::mock::ControlCall) {
        assert_eq!(c.request, wire::CLEAR_HALT_MARKER);
        assert_eq!(c.index, u16::from(crate::stream::BULK_ENDPOINT));
    }

    #[test]
    fn set_samplerate_by_index_matches_table() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8])]);
        // 2_500_000 is index 1 of the fallback table.
        device.set_samplerate(2_500_000).expect("set");
        let calls = transport.take_recorded();
        // clear_halt first, then the samplerate setter (C order).
        assert_clear_halt(&calls[0]);
        assert_in_setter(&calls[1], wire::SET_SAMPLERATE, 1);
    }

    #[test]
    fn set_samplerate_by_value_scales_for_iq() {
        let (transport, mut device) = mock_device();
        // Default type is Float32Iq: an off-table rate doubles then
        // divides by 1000 (6 MHz -> 12000).
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_samplerate(6_000_000).expect("set");
        let calls = transport.take_recorded();
        assert_clear_halt(&calls[0]);
        assert_in_setter(&calls[1], wire::SET_SAMPLERATE, 12_000);

        // A real (non-IQ) type skips the doubling (6 MHz -> 6000).
        device
            .set_sample_type(crate::commands::SampleType::Int16Real)
            .expect("type");
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_samplerate(6_000_000).expect("set");
        let calls = transport.take_recorded();
        assert_clear_halt(&calls[0]);
        assert_in_setter(&calls[1], wire::SET_SAMPLERATE, 6_000);
    }

    #[test]
    fn set_samplerate_small_values_are_raw_indices() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_samplerate(0).expect("set");
        let calls = transport.take_recorded();
        assert_clear_halt(&calls[0]);
        assert_in_setter(&calls[1], wire::SET_SAMPLERATE, 0);
    }

    #[test]
    fn gain_setters_clamp_like_c() {
        let (transport, device) = mock_device();
        // LNA clamps to 14; mixer and VGA clamp to 15.
        transport.script_reads(vec![Ok(vec![0u8]); 3]);
        device.set_lna_gain(200).expect("lna");
        device.set_mixer_gain(200).expect("mixer");
        device.set_vga_gain(200).expect("vga");
        let calls = transport.take_recorded();
        assert_in_setter(&calls[0], wire::SET_LNA_GAIN, 14);
        assert_in_setter(&calls[1], wire::SET_MIXER_GAIN, 15);
        assert_in_setter(&calls[2], wire::SET_VGA_GAIN, 15);
    }

    #[test]
    fn agc_setters_send_bool_as_byte() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8]); 2]);
        device.set_lna_agc(true).expect("lna agc");
        device.set_mixer_agc(false).expect("mixer agc");
        let calls = transport.take_recorded();
        assert_in_setter(&calls[0], wire::SET_LNA_AGC, 1);
        assert_in_setter(&calls[1], wire::SET_MIXER_AGC, 0);
    }

    #[test]
    fn linearity_gain_runs_c_sequence_with_table_values() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8]); 5]);
        // value 0 -> reversed index 21: vga 4, mixer 0, lna 0.
        device.set_linearity_gain(0).expect("linearity");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 5);
        assert_in_setter(&calls[0], wire::SET_MIXER_AGC, 0);
        assert_in_setter(&calls[1], wire::SET_LNA_AGC, 0);
        assert_in_setter(&calls[2], wire::SET_VGA_GAIN, 4);
        assert_in_setter(&calls[3], wire::SET_MIXER_GAIN, 0);
        assert_in_setter(&calls[4], wire::SET_LNA_GAIN, 0);
    }

    #[test]
    fn sensitivity_gain_clamps_and_uses_its_tables() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8]); 5]);
        // Any value >= 22 clamps to 21 -> reversed index 0:
        // vga 13, mixer 12, lna 14.
        device.set_sensitivity_gain(200).expect("sensitivity");
        let calls = transport.take_recorded();
        assert_in_setter(&calls[2], wire::SET_VGA_GAIN, 13);
        assert_in_setter(&calls[3], wire::SET_MIXER_GAIN, 12);
        assert_in_setter(&calls[4], wire::SET_LNA_GAIN, 14);
    }

    #[test]
    fn rf_bias_writes_gpio_port1_pin13() {
        let (transport, device) = mock_device();
        device.set_rf_bias(true).expect("bias");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::GPIO_WRITE);
        assert_eq!(c.value, 1);
        // port 1 << 5 | pin 13 = 45.
        assert_eq!(c.index, 45);
        assert!(c.data.is_empty());
    }

    #[test]
    fn set_packing_busy_while_streaming() {
        use crate::transport::mock::BulkRead;
        let (transport, mut device) = mock_device();
        transport.script_bulk(vec![BulkRead::Fill(0)]);
        device
            .set_sample_type(crate::commands::SampleType::Raw)
            .expect("type");
        device.start_rx(|_| true).expect("start");
        assert!(matches!(device.set_packing(true), Err(crate::Error::Busy)));
        device.stop_rx().expect("stop");
        assert!(!device.packing_enabled, "flag untouched on Busy");
    }

    #[test]
    fn in_setter_zero_length_status_maps_to_length_mismatch() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![])]); // 0 of 1 status byte
        let err = device.set_lna_gain(1).expect_err("short status");
        assert!(matches!(
            err,
            crate::Error::TransferLengthMismatch {
                expected: 1,
                actual: 0
            }
        ));
    }

    #[test]
    fn set_packing_updates_flag_and_wire() {
        let (transport, mut device) = mock_device();
        transport.script_reads(vec![Ok(vec![0u8])]);
        device.set_packing(true).expect("packing");
        assert!(device.packing_enabled);
        assert_in_setter(&transport.take_recorded()[0], wire::SET_PACKING, 1);
    }
}
