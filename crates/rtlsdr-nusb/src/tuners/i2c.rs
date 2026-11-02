//! A tuner's registers through the RTL2832U's I2C repeater, as every tuner
//! driver here uses them.

use crate::{Result, protocol::Com, usb::ControlBackend};

/// How a chip's registers read back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Reads {
    /// One register at a time, as written (Elonics E4000).
    Each,
    /// Always from register 0, each byte's bits reversed (Rafael Micro R82xx).
    FromZeroReversed,
}

/// One tuner's registers, with a copy of what was written so that a write
/// that changes nothing isn't sent: every control request served while
/// samples flow costs samples (sdr-rtl findings 16), and a retune or gain
/// change is made inside the stream's halt, which each request lengthens.
/// `volatile` registers hold bits the chip changes itself (a PLL's lock, an
/// AGC's gain): read every time, never taken from the copy.
#[derive(Debug)]
pub(crate) struct Registers {
    address: u8,
    reads: Reads,
    volatile: &'static [u8],
    values: [Option<u8>; 256],
}

impl Registers {
    pub fn new(address: u8, reads: Reads, volatile: &'static [u8]) -> Self {
        Self {
            address,
            reads,
            volatile,
            values: [None; 256],
        }
    }
    /// These, taken to hold `image` from register `first` on (a chip's
    /// power-on values, or what its open writes).
    pub fn holding(mut self, first: u8, image: &[u8]) -> Self {
        for (i, &value) in image.iter().enumerate() {
            self.values[usize::from(first) + i] = Some(value);
        }
        self
    }
    /// What the copy holds for `reg`.
    pub fn get(&self, reg: u8) -> Option<u8> {
        self.values[usize::from(reg)]
    }
    /// `count` registers from register 0, as the chip holds them now
    /// ([`Reads::FromZeroReversed`]: its status registers).
    pub async fn read_from_zero<C: ControlBackend>(&self, com: &Com<'_, C>, count: u16) -> Result<Vec<u8>> {
        debug_assert_eq!(self.reads, Reads::FromZeroReversed);
        Ok(com
            .i2c_read(self.address, 0, count)
            .await?
            .into_iter()
            .map(u8::reverse_bits)
            .collect())
    }
    /// `reg` as the chip holds it now.
    pub async fn read<C: ControlBackend>(&self, com: &Com<'_, C>, reg: u8) -> Result<u8> {
        Ok(match self.reads {
            Reads::Each => com.i2c_read(self.address, reg, 1).await?[0],
            Reads::FromZeroReversed => self.read_from_zero(com, u16::from(reg) + 1).await?[usize::from(reg)],
        })
    }
    /// All of `value` into `reg`, whatever the copy says (an initialization).
    pub async fn put<C: ControlBackend>(&mut self, com: &Com<'_, C>, reg: u8, value: u8) -> Result<()> {
        com.i2c_write(self.address, reg, value).await?;
        self.remember(reg, value);
        Ok(())
    }
    /// The `mask` bits of `reg` set to `value`'s, the others kept: what they
    /// are comes from the copy (or the chip, for a volatile register or one
    /// not known yet), and nothing is sent if nothing changes.
    pub async fn write<C: ControlBackend>(&mut self, com: &Com<'_, C>, reg: u8, value: u8, mask: u8) -> Result<()> {
        let current = match self.get(reg) {
            Some(v) if !self.volatile.contains(&reg) => v,
            _ => self.read(com, reg).await?,
        };
        let next = (current & !mask) | (value & mask);
        if next == current {
            self.remember(reg, current);
            return Ok(());
        }
        self.put(com, reg, next).await
    }
    /// Each (register, value, mask) in turn ([`Registers::write`]).
    pub async fn write_many<C: ControlBackend>(&mut self, com: &Com<'_, C>, rows: &[(u8, u8, u8)]) -> Result<()> {
        for &(reg, value, mask) in rows {
            self.write(com, reg, value, mask).await?;
        }
        Ok(())
    }
    fn remember(&mut self, reg: u8, value: u8) {
        if !self.volatile.contains(&reg) {
            self.values[usize::from(reg)] = Some(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeTransport;
    use futures_lite::future::block_on;
    use std::sync::atomic::Ordering;

    /// A known register is written from the copy, and only when it changes;
    /// an unknown one is read first; a volatile one is read every time.
    #[test]
    fn writes_only_what_changes_and_reads_only_what_it_must() {
        let control = FakeTransport::default();
        control.state.tuner_address.store(0xc8, Ordering::SeqCst);
        let com = Com(&control);
        let mut regs = Registers::new(0xc8, Reads::Each, &[0x07]).holding(0x10, &[0xf0]);
        let traffic = || (control.state.control_out_requests.lock().unwrap().len(), control.state.control_in_requests.lock().unwrap().len());
        block_on(regs.write(&com, 0x10, 0x0f, 0x0f)).unwrap();
        assert_eq!(traffic(), (1, 0), "known: written from the copy, not read");
        block_on(regs.write(&com, 0x10, 0x0f, 0x0f)).unwrap();
        assert_eq!(traffic(), (1, 0), "no change: nothing sent");
        block_on(regs.write(&com, 0x11, 0x01, 0x01)).unwrap();
        assert_eq!(traffic(), (3, 1), "unknown: read (pointer, read), then written");
        block_on(regs.write(&com, 0x07, 0x02, 0x02)).unwrap();
        block_on(regs.write(&com, 0x07, 0x02, 0x02)).unwrap();
        assert_eq!(regs.get(0x07), None, "volatile: never taken from the copy");
        assert_eq!(control.state.control_in_requests.lock().unwrap().len(), 3, "volatile: read each time");
    }
}
