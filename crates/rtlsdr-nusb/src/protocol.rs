//! RTL2832U USB register encoding and ordered I2C access.

use crate::{Error, Result, usb::ControlBackend};
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use std::time::Duration;

pub(crate) const RX_ENDPOINT: u8 = 0x81;
pub(crate) const TRANSFER_COUNT: usize = 8;
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
    pub(crate) fn usb_u16(reg: u16, value: u16) -> Self {
        Self {
            value: reg,
            index: 0x110,
            data: value.to_le_bytes().to_vec(),
        }
    }
}

/// Caller owns the session's control lease for the entire multi-transfer operation.
pub(crate) struct Com<'a, C: ControlBackend>(pub &'a C);
impl<C: ControlBackend> Com<'_, C> {
    async fn read(&self, value: u16, index: u16, length: u16) -> Result<Vec<u8>> {
        let mut bytes = self
            .0
            .control_in(InRequest {
                value,
                index,
                length,
            })
            .await?;
        if bytes.len() < usize::from(length) {
            return Err(Error::protocol("control read", "short USB response"));
        }
        bytes.truncate(usize::from(length));
        Ok(bytes)
    }
    async fn write(&self, value: u16, index: u16, data: &[u8]) -> Result<()> {
        self.0
            .control_out(OutRequest {
                value,
                index: index | 0x10,
                data: data.to_vec(),
            })
            .await
    }
    pub async fn usb_u8(&self, reg: u16, value: u8) -> Result<()> {
        self.write(reg, 0x100, &[value]).await
    }
    pub async fn usb_u16(&self, reg: u16, value: u16) -> Result<()> {
        self.0.control_out(OutRequest::usb_u16(reg, value)).await
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
    pub async fn receiver(&self, enabled: bool) -> Result<()> {
        self.usb_u16(0x2148, 0x0210).await?;
        if enabled {
            self.usb_u16(0x2148, 0).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeTransport;
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
        let reads = control.state.control_in_requests.lock().unwrap();
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
