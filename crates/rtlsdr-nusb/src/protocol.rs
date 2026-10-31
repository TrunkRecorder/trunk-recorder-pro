//! RTL2832U USB register encoding and ordered I2C access.

use crate::{Error, Result, usb::ControlBackend};
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use std::{future::IntoFuture, sync::atomic::Ordering, time::Duration};

pub(crate) const RX_ENDPOINT: u8 = 0x81;
pub(crate) fn timeout() -> Duration {
    Duration::from_millis(500)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InRequest {
    pub value: u16,
    pub index: u16,
    pub length: u16,
}
impl InRequest {
    pub(crate) fn encode(&self) -> ControlIn {
        ControlIn {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: 0,
            value: self.value,
            index: self.index,
            length: if cfg!(target_arch = "wasm32") {
                self.length.max(8)
            } else {
                self.length
            },
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OutRequest {
    pub value: u16,
    pub index: u16,
    pub data: Vec<u8>,
}
impl OutRequest {
    pub(crate) fn encode(&self) -> ControlOut<'_> {
        ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: 0,
            value: self.value,
            index: self.index,
            data: &self.data,
        }
    }
}

/// How often a request the chip refused (STALL) is tried again, and how far
/// apart. Now and then the RTL2832U refuses a request (sdr-rtl findings
/// 16.4-16.7); on some Linux hosts it refuses every demodulator write that no
/// control read shortly preceded (findings 21: a read 280 ms before let it
/// through, 320 ms did not; USB and system writes were never refused). Every
/// request here is safe to repeat.
const STALL_RETRIES: u32 = 3;
#[cfg(not(target_arch = "wasm32"))]
const STALL_RETRY_WAIT: Duration = Duration::from_millis(1);
/// A demodulator write with no read answered for this long is preceded by a
/// wake read, well inside findings 21's ~300 ms.
const DEMOD_WRITE_READ_WITHIN: Duration = Duration::from_millis(100);

/// USB-block register 0x1C, the test-mode register: a write there wedges the
/// chip until its power is removed (sdr-rtl's HAZARD-usb-0x1C; twice it took
/// a whole hub branch off the bus). Never written.
const USB_TEST_MODE: u16 = 0x201c;
/// The tuner's R15 bit 4, CLK_OUT_ENB: set, the tuner stops clocking the
/// demodulator and USB ends until a power cycle (sdr-rtl's image.rs). Never set.
const R15: u8 = 15;
const CLK_OUT_ENB: u8 = 1 << 4;

/// The chip's dropped-block counter: USB-block 0x163, the 188-byte blocks the
/// endpoint FIFO overflowed since it was cleared, saturating at 255 (findings
/// 16). Read only while nothing flows: a control request served mid-stream
/// costs samples itself.
pub(crate) const DROPPED_BLOCKS: u16 = 0x2163;
pub(crate) const DROPPED_BLOCK_SAMPLES: u64 = 94;

pub(crate) fn stalled(e: &Error) -> bool {
    match e {
        Error::Transfer(nusb::transfer::TransferError::Stall) => true,
        Error::Operation { source, .. } => stalled(source),
        _ => false,
    }
}

/// Caller owns the session's control lease for the entire multi-transfer operation.
pub(crate) struct Com<'a, C: ControlBackend>(pub &'a C);
impl<C: ControlBackend> Com<'_, C> {
    /// `request`, tried again when refused (counted). A refused demodulator
    /// write is let through by a read first.
    async fn retrying<T, F: IntoFuture<Output = Result<T>>>(&self, demod_write: bool, request: impl Fn() -> F) -> Result<T> {
        let mut tries = 0;
        loop {
            match request().await {
                Err(e) if tries < STALL_RETRIES && stalled(&e) => {
                    tries += 1;
                    self.0.link().stall_retries.fetch_add(1, Ordering::Relaxed);
                    #[cfg(not(target_arch = "wasm32"))]
                    std::thread::sleep(STALL_RETRY_WAIT);
                    if demod_write {
                        let _ = self.wake().await;
                    }
                }
                r => return r,
            }
        }
    }
    /// One read of DEMOD_CTL, sent only so the demodulator write after it is
    /// not refused; counted.
    async fn wake(&self) -> Result<()> {
        self.0.link().wake_reads.fetch_add(1, Ordering::Relaxed);
        self.0.control_in(InRequest { value: 0x3000, index: 0x200, length: 1 }).await?;
        self.0.link().read_answered();
        Ok(())
    }
    async fn read(&self, value: u16, index: u16, length: u16) -> Result<Vec<u8>> {
        let mut bytes = self
            .retrying(false, || {
                self.0.control_in(InRequest {
                    value,
                    index,
                    length,
                })
            })
            .await?;
        self.0.link().read_answered();
        if bytes.len() < usize::from(length) {
            return Err(Error::protocol("control read", "short USB response"));
        }
        bytes.truncate(usize::from(length));
        Ok(bytes)
    }
    async fn write(&self, value: u16, index: u16, data: &[u8]) -> Result<()> {
        // (Index 0-0x0f: a demodulator page; 0x100 USB, 0x200 system, 0x600 I2C.)
        let demod = index < 0x100;
        if index == 0x100 && value == USB_TEST_MODE {
            return Err(Error::protocol("USB register 0x1C", "the test-mode register wedges the chip until its power is removed; never written"));
        }
        if demod && !self.0.link().read_within(DEMOD_WRITE_READ_WITHIN) {
            self.wake().await?;
        }
        self.retrying(demod, || {
            self.0.control_out(OutRequest {
                value,
                index: index | 0x10,
                data: data.to_vec(),
            })
        })
        .await
    }
    pub async fn usb_u8(&self, reg: u16, value: u8) -> Result<()> {
        self.write(reg, 0x100, &[value]).await
    }
    pub async fn usb_read8(&self, reg: u16) -> Result<u8> {
        Ok(self.read(reg, 0x100, 1).await?[0])
    }
    pub async fn usb_u16(&self, reg: u16, value: u16) -> Result<()> {
        self.write(reg, 0x100, &value.to_le_bytes()).await
    }
    pub async fn sys_write(&self, reg: u16, value: u8) -> Result<()> {
        self.write(reg, 0x200, &[value]).await
    }
    pub async fn sys_read(&self, reg: u16) -> Result<u8> {
        Ok(self.read(reg, 0x200, 1).await?[0])
    }
    pub async fn demod(&self, page: u8, reg: u8, value: u16, width: usize) -> Result<()> {
        let bytes = value.to_be_bytes();
        self.write(
            (u16::from(reg) << 8) | 0x20,
            page.into(),
            &bytes[2 - width..],
        )
        .await?;
        // Required demodulator write synchronization, not a value readback.
        self.read(0x0120, 0x0a, 1).await?;
        Ok(())
    }
    pub async fn i2c_gate(&self, open: bool) -> Result<()> {
        self.demod(1, 1, if open { 0x18 } else { 0x10 }, 1).await
    }
    pub async fn i2c_write(&self, address: u8, reg: u8, value: u8) -> Result<()> {
        if reg == R15 && value & CLK_OUT_ENB != 0 {
            return Err(Error::protocol("tuner R15", "CLK_OUT_ENB stops the clock the demodulator runs on, until a power cycle; never set"));
        }
        self.write(address.into(), 0x600, &[reg, value]).await
    }
    pub async fn i2c_read(&self, address: u8, reg: u8, length: u16) -> Result<Vec<u8>> {
        self.write(address.into(), 0x600, &[reg]).await?;
        self.read(address.into(), 0x600, length).await
    }
    pub async fn gpio(&self, pin: u8, high: bool) -> Result<()> {
        let mask = 1 << pin;
        let latch = self.sys_read(0x3001).await?;
        self.sys_write(0x3001, if high { latch | mask } else { latch & !mask })
            .await?;
        let direction = self.sys_read(0x3004).await?;
        self.sys_write(0x3004, direction & !mask).await?;
        let output = self.sys_read(0x3003).await?;
        self.sys_write(0x3003, output | mask).await
    }
    /// The endpoint's FIFO reset and held (EPA_CTL 0x0210: FIFO reset and
    /// stall, librtlsdr's "reset buffer"), or released.
    pub async fn endpoint(&self, released: bool) -> Result<()> {
        self.usb_u16(0x2148, if released { 0 } else { 0x0210 }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeTransport;
    use std::sync::atomic::Ordering;

    /// The two writes that wedge the chip until its power is removed are
    /// refused, and never sent.
    #[test]
    fn wedging_writes_are_refused() {
        let control = FakeTransport::default();
        assert!(futures_lite::future::block_on(Com(&control).usb_u8(USB_TEST_MODE, 1)).is_err());
        assert!(futures_lite::future::block_on(Com(&control).i2c_write(0x34, R15, 0x68 | CLK_OUT_ENB)).is_err());
        assert!(control.state.control_out_requests.lock().unwrap().is_empty());
        futures_lite::future::block_on(Com(&control).i2c_write(0x34, R15, 0x68)).unwrap();
    }

    /// A refused demodulator write is let through by a read and tried again;
    /// refused more than STALL_RETRIES times, it fails.
    #[test]
    fn refused_requests_are_retried_after_a_wake_read() {
        let control = FakeTransport::default();
        control.state.stall_control_out_at.store(1, Ordering::SeqCst);
        control.state.stall_control_out_times.store(2, Ordering::SeqCst);
        futures_lite::future::block_on(Com(&control).demod(1, 0x15, 1, 1)).unwrap();
        let writes = control.state.control_out_requests.lock().unwrap().len();
        assert_eq!(writes, 3, "refused twice, then through");
        let reads = control.state.control_in_requests.lock().unwrap();
        assert_eq!(reads.iter().filter(|r| r.value == 0x3000).count(), 3, "one before the first write (none for 100 ms), one before each retry");
        assert_eq!(control.link.stall_retries.load(Ordering::Relaxed), 2);

        let control = FakeTransport::default();
        control.state.stall_control_out_at.store(1, Ordering::SeqCst);
        control.state.stall_control_out_times.store(STALL_RETRIES as usize + 1, Ordering::SeqCst);
        let e = futures_lite::future::block_on(Com(&control).usb_u16(0x2148, 0)).unwrap_err();
        assert!(stalled(&e));
        assert!(control.state.control_in_requests.lock().unwrap().is_empty(), "no wake read for a USB-block write");
    }

    #[test]
    fn control_encoding_has_correct_spaces_endianness_and_demod_sync() {
        let control = FakeTransport::default();
        futures_lite::future::block_on(async {
            let com = Com(&control);
            com.usb_u16(0x2158, 0x0200).await.unwrap();
            com.demod(1, 0x9f, 0x1234, 2).await.unwrap();
            com.i2c_read(0x34, 0, 3).await.unwrap();
        });
        let writes = control.state.control_out_requests.lock().unwrap();
        assert_eq!(
            writes[0],
            OutRequest {
                value: 0x2158,
                index: 0x110,
                data: vec![0, 2]
            }
        );
        assert_eq!(
            writes[1],
            OutRequest {
                value: 0x9f20,
                index: 0x11,
                data: vec![0x12, 0x34]
            }
        );
        assert_eq!(
            writes[2],
            OutRequest {
                value: 0x34,
                index: 0x610,
                data: vec![0]
            }
        );
        let mut reads = control.state.control_in_requests.lock().unwrap().clone();
        // The first demodulator write, with no read before it: a wake read first.
        assert_eq!(reads.remove(0), InRequest { value: 0x3000, index: 0x200, length: 1 });
        assert_eq!(
            reads[0],
            InRequest {
                value: 0x0120,
                index: 0x0a,
                length: 1
            }
        );
        assert_eq!(
            reads[1],
            InRequest {
                value: 0x34,
                index: 0x600,
                length: 3
            }
        );
        let encoded = writes[1].encode();
        assert_eq!(encoded.request, 0);
        assert_eq!(encoded.control_type, ControlType::Vendor);
        assert_eq!(encoded.recipient, Recipient::Device);
    }
    #[test]
    fn short_control_responses_are_errors() {
        let control = FakeTransport::default();
        control
            .state
            .responses
            .lock()
            .unwrap()
            .push_back(Ok(vec![]));
        assert!(matches!(
            futures_lite::future::block_on(Com(&control).sys_read(0x3001)),
            Err(Error::Protocol { .. })
        ));
    }
    #[test]
    fn gpio_preserves_unrelated_pins_and_sets_latch_before_output_enable() {
        let control = FakeTransport::default();
        control.state.sys_registers.lock().unwrap().extend([
            (0x3001, 0xa0),
            (0x3004, 0xff),
            (0x3003, 0x20),
        ]);
        futures_lite::future::block_on(Com(&control).gpio(0, true)).unwrap();
        let writes = control.state.control_out_requests.lock().unwrap();
        assert_eq!(
            writes
                .iter()
                .map(|r| (r.value, r.data[0]))
                .collect::<Vec<_>>(),
            [(0x3001, 0xa1), (0x3004, 0xfe), (0x3003, 0x21)]
        );
    }
}
