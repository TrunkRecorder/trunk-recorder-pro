//! What libhydrasdr has that libairspy hasn't: the device-info block an RFOne describes itself
//! with (its model, serial, inputs and what it can set), and one gain call taking the setting's
//! type. The rest of the driver is libairspy's ([`super`]).
//!
//! Both firmwares work: v1.0.0-rc2, which RFOnes shipped with, enumerates with the Airspy's USB ID
//! (1d50:60a1, board id 0), and updated firmware (v1.0.2) with HydraSDR's own (38af:0001, board id
//! 1); libhydrasdr knows both. With the Airspy's, libairspy doesn't list it (its
//! serial string isn't an Airspy's) but opens it as "the first" when it comes first; libhydrasdr
//! opens only HydraSDR firmware, never an Airspy.

use std::ffi::{c_char, c_int};

use super::{Can, SerialRead, Setting};

/// hydrasdr_gain_type_t, for a setting.
pub(super) fn gain_type(s: Setting) -> c_int {
    match s {
        Setting::Lna => 0,
        Setting::Mixer => 2,
        Setting::Vga => 4,
        Setting::Linearity => 5,
        Setting::Sensitivity => 6,
        Setting::LnaAgc => 7,
        Setting::MixerAgc => 9,
    }
}

// hydrasdr_capability_t.
const CAP_BIAS_TEE: u32 = 1 << 11;
const CAP_PACKING: u32 = 1 << 12;
const CAP_RF_PORT_SELECT: u32 = 1 << 13;

/// hydrasdr_gain_range_t.
#[repr(C)]
#[derive(Clone, Copy)]
struct GainRange {
    min: u8,
    max: u8,
    step: u8,
    default: u8,
    _reserved: [u8; 4],
}

/// hydrasdr_rf_port_info_t.
#[repr(C)]
#[derive(Clone, Copy)]
struct PortInfo {
    name: [c_char; 16],
    min_frequency: u64,
    max_frequency: u64,
    has_bias_tee: u8,
    _reserved1: [u8; 7],
    bias_tee_volts: f32,
    bias_tee_max_ma: f32,
    _reserved2: [u8; 8],
}

/// hydrasdr_device_info_t (4096 bytes), as far as it's read here; the rest is kept as bytes.
#[repr(C)]
pub(super) struct DeviceInfo {
    struct_version: [u16; 4],
    board_id: u8,
    _board_reserved: [u8; 7],
    board_name: [c_char; 32],
    firmware_version: [c_char; 128],
    pub part_serial: SerialRead,
    features: u32,
    _features_reserved: [u32; 3],
    /// LNA at 0, RF 5, mixer 10, filter 15, VGA 20, linearity 25, sensitivity 26 (the others reserved).
    gains: [GainRange; 31],
    /// The RF front end's and the clock generator's names.
    _components: [u8; 8 + 2 * 64],
    min_frequency: u64,
    max_frequency: u64,
    _freq_reserved: [u32; 8],
    rf_ports: u32,
    rf_port_count: u8,
    _rf_port_reserved: [u8; 3],
    rf_port_info: [PortInfo; 32],
    _rest: [u8; 4096 - 2448],
}

const _: () = {
    assert!(std::mem::size_of::<PortInfo>() == 56);
    assert!(std::mem::offset_of!(DeviceInfo, firmware_version) == 48);
    assert!(std::mem::offset_of!(DeviceInfo, features) == 200);
    assert!(std::mem::offset_of!(DeviceInfo, min_frequency) == 600);
    assert!(std::mem::offset_of!(DeviceInfo, rf_port_info) == 656);
    assert!(std::mem::size_of::<DeviceInfo>() == 4096);
};

impl DeviceInfo {
    pub fn zeroed() -> Box<DeviceInfo> {
        // SAFETY: plain integers, floats and arrays of them: all zeroes is a value.
        unsafe { Box::new(std::mem::zeroed()) }
    }
    /// "RFOne", from "HydraSDR RFOne".
    pub fn model(&self) -> String {
        // SAFETY: the library NUL-terminates it (and the struct was zeroed).
        let name = unsafe { super::super::cstr(self.board_name.as_ptr()) };
        name.trim().strip_prefix("HydraSDR").unwrap_or(&name).trim().to_string()
    }
    /// What it can set.
    pub fn can(&self) -> Can {
        Can { input: self.features & CAP_RF_PORT_SELECT != 0, bias_tee: self.features & CAP_BIAS_TEE != 0, packing: self.features & CAP_PACKING != 0 }
    }
    /// Its inputs ("ANT", "CABLE1", "CABLE2"), by number, and whether each has the bias-T.
    pub fn inputs(&self) -> Vec<(String, bool)> {
        let bias_tee = self.can().bias_tee;
        self.rf_port_info[..(self.rf_port_count as usize).min(32)]
            .iter()
            // SAFETY: as above.
            .map(|p| (unsafe { super::super::cstr(p.name.as_ptr()) }, bias_tee && p.has_bias_tee == 1))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_from_board_name() {
        let mut info = DeviceInfo::zeroed();
        for (i, b) in b"HydraSDR RFOne".iter().enumerate() {
            info.board_name[i] = *b as c_char;
        }
        assert_eq!(info.model(), "RFOne");
    }
}
