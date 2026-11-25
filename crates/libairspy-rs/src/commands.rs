//! Wire-level constants and enums shared with the Airspy firmware,
//! transcribed from `airspy_commands.h` and `airspy.h`.
//!
//! Every discriminant here crosses the USB wire — values must match the
//! C headers exactly (enforced by the tests at the bottom).

/// `AIRSPY_CMD_MAX` — highest vendor-request value.
pub const CMD_MAX: u8 = 27;

/// `AIRSPY_CONF_CMD_SHIFT_BIT` — the samplerate-configuration commands
/// pack extra flags above this many bits of samplerate index (up to
/// 8 samplerates).
pub const CONF_CMD_SHIFT_BIT: u8 = 3;

/// USB vendor requests shared between firmware and host
/// (`airspy_vendor_request`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// `AIRSPY_INVALID`
    Invalid = 0,
    /// `AIRSPY_RECEIVER_MODE`
    ReceiverMode = 1,
    /// `AIRSPY_SI5351C_WRITE`
    Si5351cWrite = 2,
    /// `AIRSPY_SI5351C_READ`
    Si5351cRead = 3,
    /// `AIRSPY_R820T_WRITE`
    R820tWrite = 4,
    /// `AIRSPY_R820T_READ`
    R820tRead = 5,
    /// `AIRSPY_SPIFLASH_ERASE`
    SpiflashErase = 6,
    /// `AIRSPY_SPIFLASH_WRITE`
    SpiflashWrite = 7,
    /// `AIRSPY_SPIFLASH_READ`
    SpiflashRead = 8,
    /// `AIRSPY_BOARD_ID_READ`
    BoardIdRead = 9,
    /// `AIRSPY_VERSION_STRING_READ`
    VersionStringRead = 10,
    /// `AIRSPY_BOARD_PARTID_SERIALNO_READ`
    BoardPartIdSerialNoRead = 11,
    /// `AIRSPY_SET_SAMPLERATE`
    SetSamplerate = 12,
    /// `AIRSPY_SET_FREQ`
    SetFreq = 13,
    /// `AIRSPY_SET_LNA_GAIN`
    SetLnaGain = 14,
    /// `AIRSPY_SET_MIXER_GAIN`
    SetMixerGain = 15,
    /// `AIRSPY_SET_VGA_GAIN`
    SetVgaGain = 16,
    /// `AIRSPY_SET_LNA_AGC`
    SetLnaAgc = 17,
    /// `AIRSPY_SET_MIXER_AGC`
    SetMixerAgc = 18,
    /// `AIRSPY_MS_VENDOR_CMD`
    MsVendorCmd = 19,
    /// `AIRSPY_SET_RF_BIAS_CMD`
    SetRfBias = 20,
    /// `AIRSPY_GPIO_WRITE`
    GpioWrite = 21,
    /// `AIRSPY_GPIO_READ`
    GpioRead = 22,
    /// `AIRSPY_GPIODIR_WRITE`
    GpiodirWrite = 23,
    /// `AIRSPY_GPIODIR_READ`
    GpiodirRead = 24,
    /// `AIRSPY_GET_SAMPLERATES`
    GetSamplerates = 25,
    /// `AIRSPY_SET_PACKING`
    SetPacking = 26,
    /// `AIRSPY_SPIFLASH_ERASE_SECTOR` (`= AIRSPY_CMD_MAX`)
    SpiflashEraseSector = 27,
}

/// Receiver mode (`receiver_mode_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReceiverMode {
    /// `RECEIVER_MODE_OFF`
    Off = 0,
    /// `RECEIVER_MODE_RX`
    Rx = 1,
}

/// Output sample format (`enum airspy_sample_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SampleType {
    /// `AIRSPY_SAMPLE_FLOAT32_IQ` — 2 × 32-bit float per sample
    Float32Iq = 0,
    /// `AIRSPY_SAMPLE_FLOAT32_REAL` — 1 × 32-bit float per sample
    Float32Real = 1,
    /// `AIRSPY_SAMPLE_INT16_IQ` — 2 × 16-bit int per sample
    Int16Iq = 2,
    /// `AIRSPY_SAMPLE_INT16_REAL` — 1 × 16-bit int per sample
    Int16Real = 3,
    /// `AIRSPY_SAMPLE_UINT16_REAL` — 1 × 16-bit unsigned int per sample
    Uint16Real = 4,
    /// `AIRSPY_SAMPLE_RAW` — raw packed samples from the device
    Raw = 5,
}

impl SampleType {
    /// Number of supported sample types (`AIRSPY_SAMPLE_END`).
    pub const COUNT: u8 = 6;
}

/// Board identifier (`enum airspy_board_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BoardId {
    /// `AIRSPY_BOARD_ID_PROTO_AIRSPY`
    ProtoAirspy = 0,
    /// `AIRSPY_BOARD_ID_INVALID`
    Invalid = 0xFF,
}

impl BoardId {
    /// Decode a board id byte read from the device. Returns `None` for
    /// values the C library would print as "Unknown Board ID".
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::ProtoAirspy),
            0xFF => Some(Self::Invalid),
            _ => None,
        }
    }

    /// The display name, exactly as `airspy_board_id_name()` returns it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ProtoAirspy => "AIRSPY",
            Self::Invalid => "Invalid Board ID",
        }
    }
}

/// GPIO port (`airspy_gpio_port_t`), ports 0–7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum GpioPort {
    Port0 = 0,
    Port1 = 1,
    Port2 = 2,
    Port3 = 3,
    Port4 = 4,
    Port5 = 5,
    Port6 = 6,
    Port7 = 7,
}

/// GPIO pin (`airspy_gpio_pin_t`), pins 0–31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum GpioPin {
    Pin0 = 0,
    Pin1 = 1,
    Pin2 = 2,
    Pin3 = 3,
    Pin4 = 4,
    Pin5 = 5,
    Pin6 = 6,
    Pin7 = 7,
    Pin8 = 8,
    Pin9 = 9,
    Pin10 = 10,
    Pin11 = 11,
    Pin12 = 12,
    Pin13 = 13,
    Pin14 = 14,
    Pin15 = 15,
    Pin16 = 16,
    Pin17 = 17,
    Pin18 = 18,
    Pin19 = 19,
    Pin20 = 20,
    Pin21 = 21,
    Pin22 = 22,
    Pin23 = 23,
    Pin24 = 24,
    Pin25 = 25,
    Pin26 = 26,
    Pin27 = 27,
    Pin28 = 28,
    Pin29 = 29,
    Pin30 = 30,
    Pin31 = 31,
}

/// Device configuration page (`airspy_common_config_pages_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConfigPage {
    /// `CONFIG_CALIBRATION`
    Calibration = 0,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_request_bytes_match_c_airspy_vendor_request() {
        // Wire compatibility with `airspy_vendor_request` in
        // airspy_commands.h — these bytes go over USB.
        assert_eq!(Command::Invalid as u8, 0);
        assert_eq!(Command::ReceiverMode as u8, 1);
        assert_eq!(Command::Si5351cWrite as u8, 2);
        assert_eq!(Command::Si5351cRead as u8, 3);
        assert_eq!(Command::R820tWrite as u8, 4);
        assert_eq!(Command::R820tRead as u8, 5);
        assert_eq!(Command::SpiflashErase as u8, 6);
        assert_eq!(Command::SpiflashWrite as u8, 7);
        assert_eq!(Command::SpiflashRead as u8, 8);
        assert_eq!(Command::BoardIdRead as u8, 9);
        assert_eq!(Command::VersionStringRead as u8, 10);
        assert_eq!(Command::BoardPartIdSerialNoRead as u8, 11);
        assert_eq!(Command::SetSamplerate as u8, 12);
        assert_eq!(Command::SetFreq as u8, 13);
        assert_eq!(Command::SetLnaGain as u8, 14);
        assert_eq!(Command::SetMixerGain as u8, 15);
        assert_eq!(Command::SetVgaGain as u8, 16);
        assert_eq!(Command::SetLnaAgc as u8, 17);
        assert_eq!(Command::SetMixerAgc as u8, 18);
        assert_eq!(Command::MsVendorCmd as u8, 19);
        assert_eq!(Command::SetRfBias as u8, 20);
        assert_eq!(Command::GpioWrite as u8, 21);
        assert_eq!(Command::GpioRead as u8, 22);
        assert_eq!(Command::GpiodirWrite as u8, 23);
        assert_eq!(Command::GpiodirRead as u8, 24);
        assert_eq!(Command::GetSamplerates as u8, 25);
        assert_eq!(Command::SetPacking as u8, 26);
        assert_eq!(Command::SpiflashEraseSector as u8, 27);
        assert_eq!(CMD_MAX, 27);
    }

    #[test]
    fn samplerate_conf_shift_matches_c() {
        // AIRSPY_CONF_CMD_SHIFT_BIT in airspy_commands.h — used to pack
        // the packing flag alongside the samplerate index.
        assert_eq!(CONF_CMD_SHIFT_BIT, 3);
    }

    #[test]
    fn receiver_mode_values_match_c() {
        assert_eq!(ReceiverMode::Off as u8, 0);
        assert_eq!(ReceiverMode::Rx as u8, 1);
    }

    #[test]
    fn sample_type_values_match_c_enum_airspy_sample_type() {
        assert_eq!(SampleType::Float32Iq as u8, 0);
        assert_eq!(SampleType::Float32Real as u8, 1);
        assert_eq!(SampleType::Int16Iq as u8, 2);
        assert_eq!(SampleType::Int16Real as u8, 3);
        assert_eq!(SampleType::Uint16Real as u8, 4);
        assert_eq!(SampleType::Raw as u8, 5);
        assert_eq!(SampleType::COUNT, 6); // AIRSPY_SAMPLE_END
    }

    #[test]
    fn board_id_values_and_names_match_c() {
        assert_eq!(BoardId::ProtoAirspy as u8, 0);
        assert_eq!(BoardId::Invalid as u8, 0xFF);
        // Names from airspy_board_id_name() in airspy.c.
        assert_eq!(BoardId::ProtoAirspy.name(), "AIRSPY");
        assert_eq!(BoardId::Invalid.name(), "Invalid Board ID");
    }

    #[test]
    fn board_id_from_u8_roundtrips_and_rejects_unknown() {
        assert_eq!(BoardId::from_u8(0), Some(BoardId::ProtoAirspy));
        assert_eq!(BoardId::from_u8(0xFF), Some(BoardId::Invalid));
        assert_eq!(BoardId::from_u8(7), None);
    }

    #[test]
    fn gpio_port_and_pin_values_match_c() {
        assert_eq!(GpioPort::Port0 as u8, 0);
        assert_eq!(GpioPort::Port7 as u8, 7);
        assert_eq!(GpioPin::Pin0 as u8, 0);
        assert_eq!(GpioPin::Pin13 as u8, 13);
        assert_eq!(GpioPin::Pin31 as u8, 31);
    }

    #[test]
    fn config_page_values_match_c() {
        assert_eq!(ConfigPage::Calibration as u8, 0);
    }
}
