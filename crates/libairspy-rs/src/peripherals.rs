//! Peripheral register and flash access, ported from `airspy.c`:
//! si5351c / R820T register I/O, the GPIO surface, and SPI flash.
//!
//! The `airspy_config_read` / `airspy_config_write` declarations in
//! `airspy.h` have **no implementation anywhere in the reference
//! revision** (nothing in `airspy.c` or the tools references them), so
//! this port omits them rather than inventing wire behavior.

use crate::commands::{Command, GpioPin, GpioPort};
use crate::device::Device;
use crate::error::{Error, Result};
use crate::transfer::{NO_WINDEX, NO_WVALUE};

/// `airspy_spiflash_write`'s address bound: `address > 0x0FFFFF` is
/// `AIRSPY_ERROR_INVALID_PARAM` (airspy.c).
const SPIFLASH_MAX_ADDRESS: u32 = 0x000F_FFFF;

/// Bits the port number occupies above the pin in the `port_pin`
/// packing — the `((uint8_t)port) << 5` shift shared by every GPIO
/// function in airspy.c.
const GPIO_PORT_SHIFT: u8 = 5;

/// Bits the low half of a SPI-flash address occupies in `wIndex` —
/// the `address >> 16` / `address & 0xFFFF` split in
/// `airspy_spiflash_write` and `airspy_spiflash_read` (airspy.c).
const SPIFLASH_ADDR_LOW_BITS: u32 = 16;
/// Mask selecting that low half (`address & 0xFFFF` in the same C
/// functions).
const SPIFLASH_ADDR_LOW_MASK: u32 = 0xFFFF;

/// Pack `port << 5 | pin` (the `port_pin` computation shared by every
/// GPIO function in airspy.c).
fn port_pin(port: GpioPort, pin: GpioPin) -> u16 {
    u16::from((port as u8) << GPIO_PORT_SHIFT | pin as u8)
}

impl Device {
    /// Read one register-style byte: the IN pattern with the register
    /// (or port/pin) in `wIndex`, returning the byte (`result < 1` →
    /// the libusb error, as in C).
    fn in_byte(&self, command: Command, index: u16) -> Result<u8> {
        let mut value = [0u8; 1];
        let n = self.vendor_in(command, NO_WVALUE, index, &mut value)?;
        if n < value.len() {
            return Err(Error::TransferLengthMismatch {
                expected: value.len(),
                actual: n,
            });
        }
        Ok(value[0])
    }

    /// Write an si5351c clock-generator register
    /// (`airspy_si5351c_write`).
    pub fn si5351c_write(&self, register: u8, value: u8) -> Result<()> {
        self.out_setter(
            Command::Si5351cWrite,
            u16::from(value),
            u16::from(register),
            &[],
        )
    }

    /// Read an si5351c clock-generator register
    /// (`airspy_si5351c_read`).
    pub fn si5351c_read(&self, register: u8) -> Result<u8> {
        self.in_byte(Command::Si5351cRead, u16::from(register))
    }

    /// Write an R820T tuner register (`airspy_r820t_write`).
    pub fn r820t_write(&self, register: u8, value: u8) -> Result<()> {
        self.out_setter(
            Command::R820tWrite,
            u16::from(value),
            u16::from(register),
            &[],
        )
    }

    /// Read an R820T tuner register (`airspy_r820t_read`).
    pub fn r820t_read(&self, register: u8) -> Result<u8> {
        self.in_byte(Command::R820tRead, u16::from(register))
    }

    /// Read a GPIO level (`airspy_gpio_read`; 0 or 1).
    pub fn gpio_read(&self, port: GpioPort, pin: GpioPin) -> Result<u8> {
        self.in_byte(Command::GpioRead, port_pin(port, pin))
    }

    /// Set a GPIO direction (`airspy_gpiodir_write`; 0 = input,
    /// 1 = output).
    pub fn gpiodir_write(&self, port: GpioPort, pin: GpioPin, value: u8) -> Result<()> {
        self.out_setter(
            Command::GpiodirWrite,
            u16::from(value),
            port_pin(port, pin),
            &[],
        )
    }

    /// Read a GPIO direction (`airspy_gpiodir_read`).
    pub fn gpiodir_read(&self, port: GpioPort, pin: GpioPin) -> Result<u8> {
        self.in_byte(Command::GpiodirRead, port_pin(port, pin))
    }

    /// Erase the entire SPI flash (`airspy_spiflash_erase`).
    ///
    /// **Destructive**: this wipes the firmware image; only meaningful
    /// as part of a firmware-update flow with a new image ready.
    pub fn spiflash_erase(&self) -> Result<()> {
        self.out_setter(Command::SpiflashErase, NO_WVALUE, NO_WINDEX, &[])
    }

    /// Erase one SPI-flash sector (`airspy_spiflash_erase_sector`).
    ///
    /// **Destructive**. The C header documents sectors 2–13 as valid
    /// (0 and 1 are reserved) but the library does not validate, and
    /// neither does this port — the caller owns that judgment.
    pub fn spiflash_erase_sector(&self, sector: u16) -> Result<()> {
        self.out_setter(Command::SpiflashEraseSector, sector, NO_WINDEX, &[])
    }

    /// Write SPI flash (`airspy_spiflash_write`): the 20-bit address
    /// splits across `wValue` (high) and `wIndex` (low); addresses
    /// above `0x0FFFFF` are `InvalidParam` like C.
    ///
    /// **Destructive**: can corrupt firmware or calibration if
    /// misused.
    pub fn spiflash_write(&self, address: u32, data: &[u8]) -> Result<()> {
        if address > SPIFLASH_MAX_ADDRESS {
            return Err(Error::InvalidParam);
        }
        // C's length parameter is uint16_t; a larger slice cannot be
        // expressed on the wire.
        if u16::try_from(data.len()).is_err() {
            return Err(Error::InvalidParam);
        }
        #[allow(clippy::cast_possible_truncation)]
        self.out_setter(
            Command::SpiflashWrite,
            (address >> SPIFLASH_ADDR_LOW_BITS) as u16,
            (address & SPIFLASH_ADDR_LOW_MASK) as u16,
            data,
        )
    }

    /// Read SPI flash (`airspy_spiflash_read`) into `buf` (same
    /// address packing as the write; C performs no address check on
    /// reads and neither does this port).
    pub fn spiflash_read(&self, address: u32, buf: &mut [u8]) -> Result<()> {
        if u16::try_from(buf.len()).is_err() {
            return Err(Error::InvalidParam);
        }
        #[allow(clippy::cast_possible_truncation)]
        let n = self.vendor_in(
            Command::SpiflashRead,
            (address >> SPIFLASH_ADDR_LOW_BITS) as u16,
            (address & SPIFLASH_ADDR_LOW_MASK) as u16,
            buf,
        )?;
        if n < buf.len() {
            return Err(Error::TransferLengthMismatch {
                expected: buf.len(),
                actual: n,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::{GpioPin, GpioPort};
    use crate::control::tests::mock_device;
    use crate::transport::mock::wire;

    #[test]
    fn si5351c_and_r820t_register_writes() {
        let (transport, device) = mock_device();
        device.si5351c_write(0x10, 0xAB).expect("si");
        device.r820t_write(0x05, 0xCD).expect("r820t");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 2);
        for (c, (req, val, reg)) in calls.iter().zip([
            (wire::SI5351C_WRITE, 0xAB, 0x10),
            (wire::R820T_WRITE, 0xCD, 0x05),
        ]) {
            assert_eq!(c.request_type, wire::VENDOR_OUT);
            assert_eq!(c.request, req);
            assert_eq!((c.value, c.index), (val, reg));
            assert!(c.data.is_empty());
            assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
        }
    }

    #[test]
    fn si5351c_and_r820t_register_reads() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![0x5A]), Ok(vec![0xA5])]);
        assert_eq!(device.si5351c_read(0x11).expect("si"), 0x5A);
        assert_eq!(device.r820t_read(0x06).expect("r820t"), 0xA5);
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 2);
        for (c, (req, reg)) in calls
            .iter()
            .zip([(wire::SI5351C_READ, 0x11), (wire::R820T_READ, 0x06)])
        {
            assert_eq!(c.request_type, wire::VENDOR_IN);
            assert_eq!(c.request, req);
            assert_eq!((c.value, c.index), (0, reg));
            assert_eq!(c.data.len(), 1);
            assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
        }
    }

    #[test]
    fn gpio_surface_encodes_port_pin() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![1]), Ok(vec![0])]);
        device
            .gpio_write(GpioPort::Port2, GpioPin::Pin7, 1)
            .expect("write");
        assert_eq!(
            device
                .gpio_read(GpioPort::Port2, GpioPin::Pin7)
                .expect("read"),
            1
        );
        device
            .gpiodir_write(GpioPort::Port0, GpioPin::Pin31, 1)
            .expect("dir write");
        assert_eq!(
            device
                .gpiodir_read(GpioPort::Port0, GpioPin::Pin31)
                .expect("dir read"),
            0
        );
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 4);
        // Writes are vendor-OUT, reads vendor-IN, all on the shared
        // control timeout.
        for (c, direction) in calls.iter().zip([
            wire::VENDOR_OUT,
            wire::VENDOR_IN,
            wire::VENDOR_OUT,
            wire::VENDOR_IN,
        ]) {
            assert_eq!(c.request_type, direction);
            assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
        }
        // port 2 << 5 | pin 7 = 71; port 0 << 5 | pin 31 = 31.
        assert_eq!(calls[0].request, wire::GPIO_WRITE);
        assert_eq!((calls[0].value, calls[0].index), (1, 71));
        assert_eq!(calls[1].request, wire::GPIO_READ);
        assert_eq!((calls[1].value, calls[1].index), (0, 71));
        assert_eq!(calls[2].request, wire::GPIODIR_WRITE);
        assert_eq!((calls[2].value, calls[2].index), (1, 31));
        assert_eq!(calls[3].request, wire::GPIODIR_READ);
        assert_eq!((calls[3].value, calls[3].index), (0, 31));
    }

    #[test]
    fn spiflash_erase_variants() {
        let (transport, device) = mock_device();
        device.spiflash_erase().expect("erase");
        device.spiflash_erase_sector(5).expect("sector");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 2);
        for c in &calls {
            assert_eq!(c.request_type, wire::VENDOR_OUT);
            assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
            assert!(c.data.is_empty());
        }
        assert_eq!(calls[0].request, wire::SPIFLASH_ERASE);
        assert_eq!((calls[0].value, calls[0].index), (0, 0));
        assert_eq!(calls[1].request, wire::SPIFLASH_ERASE_SECTOR);
        assert_eq!((calls[1].value, calls[1].index), (5, 0));
    }

    #[test]
    fn spiflash_write_splits_address_and_validates() {
        let (transport, device) = mock_device();
        device.spiflash_write(0x0A_BCDE, &[1, 2, 3]).expect("write");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_OUT);
        assert_eq!(c.request, wire::SPIFLASH_WRITE);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
        // address >> 16 = 0x0A; address & 0xFFFF = 0xBCDE.
        assert_eq!((c.value, c.index), (0x0A, 0xBCDE));
        assert_eq!(c.data, vec![1, 2, 3]);

        // C: address > 0x0FFFFF → AIRSPY_ERROR_INVALID_PARAM, checked
        // before any transfer — nothing may reach the wire.
        assert!(matches!(
            device.spiflash_write(0x10_0000, &[0]),
            Err(crate::Error::InvalidParam)
        ));
        assert!(transport.take_recorded().is_empty());
    }

    #[test]
    fn spiflash_read_splits_address_and_fills() {
        let (transport, device) = mock_device();
        transport.script_reads(vec![Ok(vec![9, 8, 7, 6])]);
        let mut buf = [0u8; 4];
        device.spiflash_read(0x01_0002, &mut buf).expect("read");
        assert_eq!(buf, [9, 8, 7, 6]);
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.request_type, wire::VENDOR_IN);
        assert_eq!(c.request, wire::SPIFLASH_READ);
        assert_eq!(c.timeout, wire::CTRL_TIMEOUT);
        assert_eq!((c.value, c.index), (0x01, 0x0002));
        assert_eq!(c.data.len(), 4);
    }

    #[test]
    fn spiflash_length_boundary() {
        let (transport, device) = mock_device();
        // The largest length C's uint16_t parameter can express goes
        // through unchanged for both directions...
        let max_len = usize::from(u16::MAX);
        device
            .spiflash_write(0, &vec![0u8; max_len])
            .expect("max write");
        transport.script_reads(vec![Ok(vec![0xEE; max_len])]);
        let mut buf = vec![0u8; max_len];
        device.spiflash_read(0, &mut buf).expect("max read");
        let calls = transport.take_recorded();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].data.len(), max_len);
        assert_eq!(calls[1].data.len(), max_len);
        // ...and one byte past it is unrepresentable on the wire:
        // InvalidParam with nothing recorded (documented deviation —
        // the C API cannot express this length at all).
        assert!(matches!(
            device.spiflash_write(0, &vec![0u8; max_len + 1]),
            Err(crate::Error::InvalidParam)
        ));
        let mut over = vec![0u8; max_len + 1];
        assert!(matches!(
            device.spiflash_read(0, &mut over),
            Err(crate::Error::InvalidParam)
        ));
        assert!(transport.take_recorded().is_empty());
    }
}
