//! Convert interleaved eight-bit I/Q directly into caller-provided output.
use super::queue::Queue;
use crate::{Complex32, Result, maybe_future::NonWasmSend, usb::BulkInBackend};

pub trait Processor: Default + NonWasmSend + 'static {
    fn reset(&mut self);
}
#[derive(Default)]
pub struct RawProcessor;
impl Processor for RawProcessor {
    fn reset(&mut self) {}
}
#[derive(Default)]
pub struct IqProcessor {
    offset: usize,
}
impl Processor for IqProcessor {
    fn reset(&mut self) {
        self.offset = 0;
    }
}
impl IqProcessor {
    pub(super) fn has_pending(&self) -> bool {
        false
    }
    pub(super) fn process<B: BulkInBackend>(
        &mut self,
        queue: &mut Queue<B>,
        out: &mut [Complex32],
    ) -> Result<usize> {
        let Some(buffer) = queue.current.as_ref() else {
            return Ok(0);
        };
        let count = out.len().min((queue.current_len - self.offset) / 2);
        for (sample, bytes) in out[..count].iter_mut().zip(
            buffer[self.offset..queue.current_len]
                .as_chunks::<2>()
                .0
                .iter(),
        ) {
            *sample = Complex32::new(
                (f32::from(bytes[0]) - 127.5) / 127.5,
                (f32::from(bytes[1]) - 127.5) / 127.5,
            );
        }
        self.offset += count * 2;
        if self.offset == queue.current_len {
            queue.release_current();
            self.offset = 0;
        }
        Ok(count)
    }
}
