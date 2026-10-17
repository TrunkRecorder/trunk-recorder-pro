# rtlsdr-nusb

Rust-native RTL-SDR driver using [nusb](https://github.com/kevinmehall/nusb),
licensed under Apache-2.0. No librtlsdr or libusb dependency. The device, streamer,
close/shutdown and sync/async APIs follow
[hydrasdr-rs](https://github.com/bastibl/hydrasdr-rs).

## Hardware and controls

- RTL2832U with R820T/R820T2/R860-compatible or R828D tuners.
- R828D input selection and RTL-SDR Blog V4 HF upconversion/filter switching.
  V4 detection uses USB manufacturer `RTLSDRBlog` and product `Blog V4`.
- Frequency, sample rate, automatic/manual tuner gain, ppm correction, GPIO 0
  bias tee (off by default), and I/Q direct sampling on suitably wired boards.
- Typed raw unsigned eight-bit I/Q blocks or converted `Complex32` samples.
- Standard USB IDs `0bda:2832` / `0bda:2838`, index/serial selection, and custom
  VID/PID matching through the builder.

The current range is 900,001–3,200,000 complex samples/s. Rates above 2.4 MS/s
may drop samples. Tuner operation starts at 28.8 MHz; lower frequencies require
Blog V4 or a suitable direct-sampling input. The nominal upper limit is
1.766 GHz, subject to the individual tuner's PLL locking. Manual gain is an
approximation based on the reference driver's LNA/mixer steps, not librtlsdr's
gain table. Generic R828D boards currently assume a 28.8 MHz tuner crystal;
boards with a different tuner crystal are not supported.

Hardware validation so far: an R820T-family `0bda:2838` receiver on Linux, with
blocking/async capture, retuning at 100, 433.92 and 1090 MHz, sample rates of
1.024, 2.048 and 2.4 MS/s, stop/restart, raw IQ and final-owner cleanup.
R828D/Blog V4, direct sampling and bias power switching need hardware validation.
Other tuners (E4000, FC0012/13, FC2580) and EEPROM programming are not implemented.
No Seify integration is included.

## Blocking capture

Requires Rust 1.88 or newer. Add `rtlsdr-nusb` to your dependencies (or use a
local `path` dependency before the first crates.io release).

```rust,no_run
use rtlsdr_nusb::{Complex32, Device, MaybeFuture};
use std::time::Duration;

let mut device = Device::builder()
    .frequency_hz(100_000_000)
    .sample_rate_hz(2_048_000)
    .open().wait()?;
let mut rx = device.rx_stream()?;
rx.start().wait()?;

let mut samples = [Complex32::default(); 4096];
let count = rx.read(&mut samples, Some(Duration::from_secs(1))).wait()?;
println!("received {count} samples");

rx.close().wait()?;
device.shutdown().wait()?;
# Ok::<(), rtlsdr_nusb::Error>(())
```

Run `cargo run --example rx_sync` for a complete example.

## Async capture

Enable the `smol` feature for executor-independent native discovery/open/setup,
or `tokio` when using a Tokio runtime. With both enabled, nusb uses its executor-independent smol backend. Native
control transfers and streaming themselves use nusb's completion mechanism.
The blocking API needs neither feature nor a runtime.

```toml
[dependencies]
rtlsdr-nusb = { version = "0.3", features = ["smol"] }
futures-lite = "2"
```

```rust,no_run
use rtlsdr_nusb::{Complex32, Device};

futures_lite::future::block_on(async {
    let mut device = Device::builder().frequency_hz(100_000_000).open().await?;
    let mut rx = device.rx_stream()?;
    rx.start().await?;
    let mut samples = [Complex32::default(); 4096];
    let count = rx.read(&mut samples, None).await?;
    println!("received {count} samples");
    rx.close().await?;
    device.shutdown().await?;
    Ok::<(), rtlsdr_nusb::Error>(())
})?;
# Ok::<(), rtlsdr_nusb::Error>(())
```

Run `cargo run --features smol --example rx_async`.
Native callers may mix `.wait()` and `.await()` sequentially on the same stream;
both use the same persistent queue. The timeout argument applies only to
blocking reads. Wrap awaited reads in your runtime's timeout when needed;
canceling a pending read leaves the queue available for the next read.

## Raw IQ

Select `.raw_iq()` on `Device::builder()` and call
`rx.next_block(Some(timeout)).wait()?` or `rx.next_block(None).await?`.
A successful read returns `Some(SampleBlock)`, exposing `raw_bytes()` and
`sample_count()`. Bytes are interleaved `I, Q` in `0..=255`; the float mode maps
each component with `(value - 127.5) / 127.5`. The borrowed block holds one USB
buffer, which is resubmitted on the next read. Blocking timeout returns `None`.
Float reads return the number written and may fill only part of the output slice.

Eight 256 KiB USB buffers are retained per active stream. Steady-state reads
allocate neither an output buffer nor a boxed future. Statistics count observed
USB completions and controlled discards; the RTL-SDR data format has no sequence
numbers, so device-side sample loss cannot be measured by these counters.

## Ownership and cleanup

- A device permits one stream claim. Creating a stream is dormant; `start`
  enables reception. A stopped stream retains its claim and can restart.
- The stream owns a shared device session and remains usable after `Device` is
  dropped. Bias power, if enabled, stays on until device shutdown/final-owner drop.
- `close` consumes the stream, retires its queue, stops reception and releases
  the claim. Dropping even an unpolled close operation performs fallback cleanup.
- `Device::shutdown` returns `Busy` while a stream exists. After closing the
  stream, shutdown disables reception and bias power and powers down the tuner
  and demodulator. It is terminal once started, retryable after cancellation or
  failure, and idempotent after success. Unpolled shutdown has no effect.
- Final-owner drop performs best-effort cleanup: blocking on native targets,
  scheduled in the background on WebUSB. Explicit close/shutdown reports errors.
- Failed or canceled configuration leaves hardware state unknown. Reapply a
  complete `Config` to recover; the last successful configuration remains
  available from `Device::config()`. Overlapping control sequences return `Busy`.
- Queued samples can predate a live retune. Stop, configure, then restart to
  discard old submissions. The queue is retained across a normal restart.

## Platforms

Linux needs permission to access the USB device. Opening uses nusb's kernel
interface detach/claim support. Windows needs a WinUSB-compatible driver on the
RTL-SDR interface. macOS, Linux and Windows are included in the CI test matrix;
only Linux hardware operation has been tested locally.

WebUSB is compile-checked for `wasm32-unknown-unknown`. The application's
`.cargo/config.toml` must include:

```toml
[target.wasm32-unknown-unknown]
rustflags = ["--cfg=web_sys_unstable_apis"]
```

Call `Device::request_permission().await` from a browser user gesture before
opening, or use the builder's permission method for custom IDs/serial selection.
WebUSB operations are async only. Transfer cancellation is unavailable there;
restart drains old submissions, and drop cleanup finishes in a background task.
Browser/hardware operation has not yet been tested.

## Development and releases

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --doc --all-features
cargo check --lib --target wasm32-unknown-unknown --all-features
```

Hardware tests are ignored by default. With exclusive access to a receiver:

```sh
RTLSDR_SERIAL=00000001 cargo test --test hardware -- --ignored --nocapture --test-threads=1
```

CI checks formatting, Clippy, tests/docs with default/smol/tokio/all features,
Rust 1.88, WebUSB compilation and the distributable crate. A pushed `v<version>`
tag runs the same checks, verifies the Cargo version, publishes to crates.io
through trusted publishing, then creates a GitHub release containing the crate
archive.

## Acknowledgment and license

Based on [webrtlsdr](https://github.com/jtarrio/webrtlsdr). Distributed under
[Apache-2.0](LICENSE).
