> **Trunk Recorder Pro's fork** of [libairspy-rs](https://github.com/jasonherald/libairspy-rs) 0.2.1
> (BSD-3-Clause AND MIT, see `LICENSE` and `NOTICE`): the `libairspy` crate of that repository,
> kept in this workspace as the Airspy R2 / Mini's built-in driver
> (`crates/trunk-pro/src/radio/airspy/native.rs`). What's changed, to match C libairspy where
> the port didn't yet, and to fit the recorder:
>
> - **Packing off at open**, as `airspy_open_init` leaves it (a radio left packing by another
>   program would otherwise stream packed samples read as unpacked).
> - **Conversion filters** can be replaced (`set_conversion_filter_float32` / `_int16`), as
>   `airspy_set_conversion_filter_*`.
> - **The consumer can be the caller's thread**: `start_rx_polled` and `recv` hand each block to
>   a closure on the thread that asks, with a timeout, so a receiver that isn't `Send + 'static`
>   takes samples without a thread and a queue between. `start_rx` (a callback on a thread of
>   the driver's, as C's) is unchanged.
> - **Listing and opening by serial read what the operating system already knows** (the USB
>   serial string), without opening each radio: one that's streaming is listed, and isn't
>   disturbed. The descriptor is read from the device only when the system gave none.
> - **Why a radio wouldn't open is returned** (no access, in use), not "not found".
> - **Windows: the USB reader and consumer threads run at `THREAD_PRIORITY_HIGHEST`**, as C's.
> - `tracing` → `log`; the tokio and smol stream wrappers and the GPL `airspy-tools` crate
>   aren't included.
>
> Differences from C libairspy that are upstream's, and kept: a transfer error that passes is
> ridden out, not the end of the stream; the sample type can't be changed while streaming;
> `airspy_config_read` / `_write` and Android's `open_fd` have no counterpart.
>
> The IQ converters come from airspyone_host's last all-MIT revision (`bd15be38`), not its
> current ones, whose licence was changed in 2025 (see `NOTICE`): **don't port from the current
> `iqconverter_*` files.**
>
> Not yet run against hardware from this workspace: the tests in `tests/device.rs` need an
> Airspy (`cargo test -p libairspy-rs --test device -- --ignored --test-threads=1`).

# libairspy-rs

A pure-Rust port of [libairspy](https://github.com/airspy/airspyone_host), the USB driver for
the Airspy R2 and Airspy Mini, over [nusb](https://github.com/kevinmehall/nusb): no C library,
no libusb. See the crate documentation (`src/lib.rs`) for the API.
