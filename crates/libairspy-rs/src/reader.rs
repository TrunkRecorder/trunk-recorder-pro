//! Blocking `Iterator` sample delivery — the sync alternative to the
//! [`crate::Device::start_rx`] callback, mirroring librtlsdr-rs's
//! reader shape. Blocks are bridged off the consumer thread over a
//! bounded channel; when the iterator falls behind, the channel fills,
//! the consumer stalls, and the ring's C drop accounting takes over.

use std::sync::mpsc::{Receiver, SyncSender};

use crate::commands::SampleType;
use crate::conversion::Samples;
use crate::device::Device;
use crate::error::Result;
use crate::stream::{RAW_BUFFER_COUNT, Transfer};

/// An owned block of samples, the by-value counterpart of
/// [`Samples`].
#[derive(Debug, Clone)]
pub enum SampleBlock {
    /// See [`Samples::Float32`].
    Float32(Vec<f32>),
    /// See [`Samples::Int16`].
    Int16(Vec<i16>),
    /// See [`Samples::Uint16`].
    Uint16(Vec<u16>),
    /// See [`Samples::Raw`].
    Raw(Vec<u8>),
}

/// An owned delivered block — [`crate::stream::Transfer`] detached
/// from the consumer thread's buffers.
#[derive(Debug, Clone)]
pub struct OwnedTransfer {
    /// The block's samples in the latched format.
    pub samples: SampleBlock,
    /// The latched sample type.
    pub sample_type: SampleType,
    /// See [`crate::stream::Transfer::dropped_samples`].
    pub dropped_samples: u64,
}

impl From<Transfer<'_>> for OwnedTransfer {
    fn from(t: Transfer<'_>) -> Self {
        let samples = match t.samples {
            Samples::Float32(s) => SampleBlock::Float32(s.to_vec()),
            Samples::Int16(s) => SampleBlock::Int16(s.to_vec()),
            Samples::Uint16(s) => SampleBlock::Uint16(s.to_vec()),
            Samples::Raw(s) => SampleBlock::Raw(s.to_vec()),
        };
        Self {
            samples,
            sample_type: t.sample_type,
            dropped_samples: t.dropped_samples,
        }
    }
}

/// The consumer-side bridge: convert the borrowed transfer to owned
/// and hand it to the iterator's channel. Returns the callback's
/// keep-streaming flag — a dropped iterator (disconnected channel)
/// stops the stream like a `false`-returning callback.
///
/// A full channel blocks in the bounded send, stalling the consumer
/// so overflow lands in the ring's C drop accounting; the send
/// unblocks with an error the moment the receiver drops.
fn forward_transfer(tx: &SyncSender<OwnedTransfer>, transfer: Transfer<'_>) -> bool {
    tx.send(OwnedTransfer::from(transfer)).is_ok()
}

/// Blocking iterator over sample blocks; created by
/// [`Device::rx_blocks`]. Dropping it stops the stream
/// (`airspy_stop_rx` semantics).
#[derive(Debug)]
pub struct BlockIter<'d> {
    device: &'d mut Device,
    /// `Option` so `Drop` can disconnect the channel BEFORE joining
    /// the workers — a consumer blocked in a full-channel send would
    /// otherwise deadlock `stop_rx`'s join.
    rx: Option<Receiver<OwnedTransfer>>,
}

impl Iterator for BlockIter<'_> {
    type Item = OwnedTransfer;

    /// Blocks until the next sample block, or `None` once streaming
    /// stops (stop request, device error, or callback shutdown).
    fn next(&mut self) -> Option<OwnedTransfer> {
        self.rx.as_ref()?.recv().ok()
    }
}

impl Drop for BlockIter<'_> {
    fn drop(&mut self) {
        // Disconnect first: a consumer stalled in the bounded send
        // errors out immediately, so stop_rx's thread joins cannot
        // deadlock on it.
        drop(self.rx.take());
        let _ = self.device.stop_rx();
    }
}

impl Device {
    /// Start streaming and return a blocking iterator over sample
    /// blocks — the sync-pull alternative to [`Device::start_rx`],
    /// delivering the same data with owned buffers.
    ///
    /// The iterator borrows the device mutably for the stream's
    /// lifetime and stops the stream when dropped.
    pub fn rx_blocks(&mut self) -> Result<BlockIter<'_>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(RAW_BUFFER_COUNT);
        self.start_rx(move |transfer| forward_transfer(&tx, transfer))?;
        Ok(BlockIter {
            device: self,
            rx: Some(rx),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SampleType;
    use crate::conversion::Samples;
    use crate::stream::Transfer;
    use std::sync::mpsc;

    fn raw_transfer(bytes: &[u8]) -> Transfer<'_> {
        Transfer {
            samples: Samples::Raw(bytes),
            sample_type: SampleType::Raw,
            dropped_samples: 7,
        }
    }

    #[test]
    fn transfers_convert_to_owned_blocks() {
        let t = Transfer {
            samples: Samples::Int16(&[-1, 0, 1]),
            sample_type: SampleType::Int16Real,
            dropped_samples: 3,
        };
        let owned = OwnedTransfer::from(t);
        assert_eq!(owned.sample_type, SampleType::Int16Real);
        assert_eq!(owned.dropped_samples, 3);
        assert!(matches!(owned.samples, SampleBlock::Int16(ref v) if v.as_slice() == [-1, 0, 1]));
    }

    #[test]
    fn forwarding_keeps_streaming_while_receiver_lives() {
        let (tx, rx) = mpsc::sync_channel(2);
        assert!(forward_transfer(&tx, raw_transfer(&[1, 2])));
        let got = rx.recv().expect("delivered");
        assert!(matches!(got.samples, SampleBlock::Raw(ref b) if b.as_slice() == [1, 2]));
        assert_eq!(got.dropped_samples, 7);
    }

    #[test]
    fn forwarding_stops_when_receiver_dropped() {
        let (tx, rx) = mpsc::sync_channel::<OwnedTransfer>(1);
        drop(rx);
        // A dead receiver means the iterator is gone: stop streaming
        // (callback-false semantics).
        assert!(!forward_transfer(&tx, raw_transfer(&[3])));
    }

    /// Bound between "the thread is at the send call-site" (barrier)
    /// and "the send has parked on the full channel" — there is no
    /// portable way to observe a parked sender, so this is a settle
    /// window, not a correctness condition: the assertions below hold
    /// under either interleaving.
    const BLOCKED_SEND_SETTLE: core::time::Duration = core::time::Duration::from_millis(50);

    #[test]
    fn blocked_forward_unblocks_when_receiver_drops() {
        // The Drop-order regression: a consumer stalled in a full
        // channel's send must return (false) as soon as the receiver
        // disconnects, so worker joins can't deadlock.
        let (tx, rx) = mpsc::sync_channel::<OwnedTransfer>(1);
        assert!(forward_transfer(&tx, raw_transfer(&[1]))); // fills the slot
        let at_send = std::sync::Arc::new(std::sync::Barrier::new(2));
        let at_send2 = std::sync::Arc::clone(&at_send);
        let blocked = std::thread::spawn(move || {
            at_send2.wait();
            forward_transfer(&tx, raw_transfer(&[2]))
        });
        at_send.wait();
        std::thread::sleep(BLOCKED_SEND_SETTLE);
        drop(rx);
        assert!(
            !blocked.join().expect("join"),
            "send must fail after disconnect"
        );
    }

    #[test]
    fn blocked_forward_delivers_when_receiver_catches_up() {
        let (tx, rx) = mpsc::sync_channel::<OwnedTransfer>(1);
        assert!(forward_transfer(&tx, raw_transfer(&[1])));
        let at_send = std::sync::Arc::new(std::sync::Barrier::new(2));
        let at_send2 = std::sync::Arc::clone(&at_send);
        let blocked = std::thread::spawn(move || {
            at_send2.wait();
            forward_transfer(&tx, raw_transfer(&[2]))
        });
        at_send.wait();
        std::thread::sleep(BLOCKED_SEND_SETTLE);
        let first = rx.recv().expect("first");
        assert!(matches!(first.samples, SampleBlock::Raw(ref b) if b.as_slice() == [1]));
        assert!(
            blocked.join().expect("join"),
            "send completes once space frees"
        );
        let second = rx.recv().expect("second");
        assert!(matches!(second.samples, SampleBlock::Raw(ref b) if b.as_slice() == [2]));
    }

    #[test]
    fn all_sample_block_variants_convert() {
        for (samples, sample_type, check) in [
            (
                Samples::Float32(&[0.5][..]),
                SampleType::Float32Real,
                0usize,
            ),
            (Samples::Uint16(&[9][..]), SampleType::Uint16Real, 1),
            (Samples::Raw(&[8][..]), SampleType::Raw, 2),
        ] {
            let t = Transfer {
                samples,
                sample_type,
                dropped_samples: 0,
            };
            let owned = OwnedTransfer::from(t);
            assert_eq!(owned.sample_type, sample_type);
            match (check, owned.samples) {
                (0, SampleBlock::Float32(v)) => assert_eq!(v, vec![0.5]),
                (1, SampleBlock::Uint16(v)) => assert_eq!(v, vec![9]),
                (2, SampleBlock::Raw(v)) => assert_eq!(v, vec![8]),
                (_, other) => unreachable!("wrong variant: {other:?}"),
            }
        }
    }
}
