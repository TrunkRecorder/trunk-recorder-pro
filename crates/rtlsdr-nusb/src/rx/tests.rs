use super::{queue::DEFAULT_BUFFER_SIZE, receiver::Receiver};
use crate::{
    Complex32, DeviceDescriptor, Error, F32Iq, RawIq,
    config::{SampleMode, Settings},
    session::{DeviceLifecycle, RxStreamClaim, Session},
    test_support::{FakeEvent, FakeTransport},
};
use nusb::MaybeFuture;
use std::{
    future::{Future, IntoFuture},
    sync::{Arc, atomic::Ordering},
    task::{Context, Waker},
    time::Duration,
};

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor {
        index: 0,
        vid: 0x0bda,
        pid: 0x2838,
        serial: None,
        manufacturer: None,
        product: None,
    }
}
fn setup<M: SampleMode>() -> (
    FakeTransport,
    Arc<Session<FakeTransport>>,
    Receiver<FakeTransport, M>,
) {
    let control = FakeTransport::default();
    let (session, _) = Session::initialize(control.clone(), descriptor(), Settings::default())
        .wait()
        .unwrap();
    let rx = Receiver::new(RxStreamClaim::acquire(&session).unwrap());
    (control, session, rx)
}
fn pending<F: Future>(future: std::pin::Pin<&mut F>) {
    assert!(
        future
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
}

#[test]
fn claims_survive_stop_and_prevent_shutdown_until_close() {
    let (_, session, mut rx) = setup::<F32Iq>();
    assert!(matches!(RxStreamClaim::acquire(&session), Err(Error::Busy)));
    assert!(matches!(session.shutdown().wait(), Err(Error::Busy)));
    assert_eq!(session.lock().device, DeviceLifecycle::Open);
    rx.start().wait().unwrap();
    rx.stop().wait().unwrap();
    assert!(matches!(RxStreamClaim::acquire(&session), Err(Error::Busy)));
    rx.close().wait().unwrap();
    session.shutdown().wait().unwrap();
    assert!(matches!(
        RxStreamClaim::acquire(&session),
        Err(Error::DeviceClosed)
    ));
}

#[test]
fn stream_owns_session_and_final_drop_powers_down_after_queue_release() {
    let (control, session, mut rx) = setup::<RawIq>();
    let weak = Arc::downgrade(&session);
    drop(session);
    rx.start().wait().unwrap();
    assert!(rx.next_block(None).wait().unwrap().is_some());
    rx.stop().wait().unwrap();
    rx.start().wait().unwrap();
    control.state.events.lock().unwrap().clear();
    drop(rx.close()); // unpolled consuming close still owns cleanup
    assert!(weak.upgrade().is_none());
    let events = control.state.events.lock().unwrap();
    let queue = events
        .iter()
        .position(|e| *e == FakeEvent::QueueDropped)
        .unwrap();
    let power = events
        .iter()
        .position(|e| matches!(e, FakeEvent::Control(r) if r.value == 0x3000 && r.data == [0x20]))
        .unwrap();
    assert!(queue < power);
    assert_eq!(control.state.sys_registers.lock().unwrap()[&0x3001] & 1, 0);
}

#[test]
fn blocking_and_async_reads_share_conversion_offset_and_queue() {
    let (control, session, mut rx) = setup::<F32Iq>();
    rx.start().wait().unwrap();
    let mut out = [Complex32::default(); 1];
    assert_eq!(rx.read(&mut out, None).wait().unwrap(), 1);
    assert_eq!(out[0].re, -1.0);
    assert_eq!(out[0].im, (1.0 - 127.5) / 127.5);
    assert_eq!(
        futures_lite::future::block_on(rx.read(&mut out, None)).unwrap(),
        1
    );
    assert_eq!(out[0].re, (2.0 - 127.5) / 127.5);
    assert_eq!(out[0].im, (3.0 - 127.5) / 127.5);
    assert_eq!(control.state.bulk_in_count.load(Ordering::SeqCst), 1);
    assert_eq!(rx.current_stats().buffers_received, 1);
    rx.close().wait().unwrap();
    session.shutdown().wait().unwrap();
}

#[test]
fn canceled_read_keeps_pending_transfers_and_claim() {
    let (control, session, mut rx) = setup::<RawIq>();
    rx.start().wait().unwrap();
    control.state.pause_bulk.store(true, Ordering::SeqCst);
    let mut read = Box::pin(rx.next_block(None).into_future());
    pending(read.as_mut());
    drop(read);
    assert!(matches!(RxStreamClaim::acquire(&session), Err(Error::Busy)));
    control.state.pause_bulk.store(false, Ordering::SeqCst);
    let block = rx.next_block(None).wait().unwrap().unwrap();
    assert_eq!(block.raw_bytes().len(), DEFAULT_BUFFER_SIZE);
    assert_eq!(&block.raw_bytes()[..4], &[0, 1, 2, 3]);
    assert_eq!(control.state.bulk_in_count.load(Ordering::SeqCst), 1);
}

#[test]
fn short_even_completions_are_data_and_blocking_timeout_is_retryable() {
    let (_, _, mut rx) = setup::<RawIq>();
    rx.start().wait().unwrap();
    rx.queue
        .as_mut()
        .unwrap()
        .bulk_in
        .as_mut()
        .unwrap()
        .short_next = true;
    assert_eq!(
        rx.next_block(None)
            .wait()
            .unwrap()
            .unwrap()
            .raw_bytes()
            .len(),
        DEFAULT_BUFFER_SIZE - 2
    );
    rx.queue
        .as_mut()
        .unwrap()
        .bulk_in
        .as_mut()
        .unwrap()
        .timeout_next = true;
    assert!(
        rx.next_block(Some(Duration::from_millis(1)))
            .wait()
            .unwrap()
            .is_none()
    );
    assert!(rx.next_block(None).wait().unwrap().is_some());
}

#[test]
fn restart_reuses_queue_and_discards_every_old_submission() {
    let (control, _, mut rx) = setup::<RawIq>();
    rx.start().wait().unwrap();
    rx.next_block(None).wait().unwrap().unwrap();
    rx.stop().wait().unwrap();
    rx.start().wait().unwrap();
    rx.next_block(None).wait().unwrap().unwrap();
    assert_eq!(control.state.bulk_in_count.load(Ordering::SeqCst), 1);
    assert_eq!(rx.current_stats().buffers_discarded_on_restart, 8);
}

#[test]
fn failed_read_requires_stop_before_rebuilding_queue() {
    let (control, _, mut rx) = setup::<RawIq>();
    rx.start().wait().unwrap();
    control
        .state
        .fail_bulk_completion
        .store(true, Ordering::SeqCst);
    assert!(rx.next_block(None).wait().is_err());
    assert!(matches!(rx.start().wait(), Err(Error::Busy)));
    rx.stop().wait().unwrap();
    rx.start().wait().unwrap();
    assert!(rx.next_block(None).wait().unwrap().is_some());
    assert_eq!(control.state.bulk_in_count.load(Ordering::SeqCst), 2);
}

#[test]
fn canceled_start_and_stop_require_cleanup_and_can_recover() {
    let (control, _, mut rx) = setup::<RawIq>();
    control.state.pause_clear_halt.store(true, Ordering::SeqCst);
    let mut start = Box::pin(rx.start().into_future());
    pending(start.as_mut());
    drop(start);
    assert!(matches!(rx.start().wait(), Err(Error::Busy)));
    control
        .state
        .pause_clear_halt
        .store(false, Ordering::SeqCst);
    rx.stop().wait().unwrap();
    rx.start().wait().unwrap();
    control.state.pause_control_out_at.store(
        control.state.control_out_count.load(Ordering::SeqCst) + 1,
        Ordering::SeqCst,
    );
    let mut stop = Box::pin(rx.stop().into_future());
    pending(stop.as_mut());
    drop(stop);
    assert!(matches!(
        rx.next_block(None).wait(),
        Err(Error::StreamClosed { .. })
    ));
    rx.stop().wait().unwrap();
    rx.start().wait().unwrap();
    assert!(rx.next_block(None).wait().unwrap().is_some());
}

#[test]
fn lazy_shutdown_is_terminal_once_started_and_retryable_after_cancellation() {
    let (control, session, rx) = setup::<RawIq>();
    drop(rx);
    let count = control.state.control_out_count.load(Ordering::SeqCst);
    drop(session.shutdown());
    assert_eq!(
        control.state.control_out_count.load(Ordering::SeqCst),
        count
    );
    control
        .state
        .pause_control_out_at
        .store(count + 1, Ordering::SeqCst);
    let mut shutdown = Box::pin(session.shutdown().into_future());
    pending(shutdown.as_mut());
    drop(shutdown);
    assert!(matches!(session.ensure_open(), Err(Error::DeviceClosed)));
    session.shutdown().wait().unwrap();
    let count = control.state.control_out_count.load(Ordering::SeqCst);
    session.shutdown().wait().unwrap();
    assert_eq!(
        control.state.control_out_count.load(Ordering::SeqCst),
        count
    );
}

#[test]
fn failed_shutdown_still_attempts_bias_and_power_cleanup() {
    let (control, session, rx) = setup::<RawIq>();
    drop(rx);
    control
        .state
        .sys_registers
        .lock()
        .unwrap()
        .insert(0x3001, 1);
    control.state.fail_control_out_at.store(
        control.state.control_out_count.load(Ordering::SeqCst) + 1,
        Ordering::SeqCst,
    );
    assert!(session.shutdown().wait().is_err());
    let regs = control.state.sys_registers.lock().unwrap();
    assert_eq!(regs[&0x3001] & 1, 0);
    assert_eq!(regs[&0x3000], 0x20);
    drop(regs);
    session.shutdown().wait().unwrap();
}

#[test]
fn canceled_configuration_invalidates_reads_and_full_retry_recovers() {
    let (control, session, mut rx) = setup::<RawIq>();
    rx.start().wait().unwrap();
    control.state.pause_control_out_at.store(
        control.state.control_out_count.load(Ordering::SeqCst) + 1,
        Ordering::SeqCst,
    );
    let mut config = Box::pin(session.configure(Settings::default()).into_future());
    pending(config.as_mut());
    assert!(matches!(
        session.receiver_mode(true).wait(),
        Err(Error::Busy)
    ));
    drop(config);
    assert!(matches!(
        rx.next_block(None).wait(),
        Err(Error::ConfigurationUnknown)
    ));
    session.configure(Settings::default()).wait().unwrap();
    rx.stop().wait().unwrap();
    rx.start().wait().unwrap();
    assert!(rx.next_block(None).wait().unwrap().is_some());
}
