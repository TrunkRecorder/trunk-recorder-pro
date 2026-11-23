//! Radios on the network ([`super::rtl`] through `rtl_tcp`, [`super::spyserver`]): the TCP they share.

use std::io::{ErrorKind, Read};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// A connection, and a server's hello, may each take this long.
pub const CONNECT: Duration = Duration::from_secs(5);
/// How long a read waits before a stream's loop looks at `stop` and the controls again.
pub const POLL: Duration = Duration::from_millis(50);
/// A stream that has said nothing for this long has stopped.
pub const SILENT: Duration = Duration::from_secs(3);

/// A TCP connection to `target` (`host:port`): each address it names is tried in turn, `within` each.
/// Commands are small and wait for nothing, so Nagle's delay is off.
pub fn dial(name: &str, target: &str, within: Duration) -> Result<TcpStream, String> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<_> = target.to_socket_addrs().map_err(|e| format!("{name}: can't find {target}: {e}"))?.collect();
    let mut last = format!("{name}: no address for {target}");
    for a in addrs {
        match TcpStream::connect_timeout(&a, within) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = format!("{name}: can't connect to {a} ({e}): is the server running there, and its port open?"),
        }
    }
    Err(last)
}

/// Whether a read only ran out of time (WouldBlock on Unix, TimedOut on Windows) or was interrupted: try again.
pub fn timed_out(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

/// Reads until `buf` is full, the server hangs up or `within` passes: how many bytes came, and whether
/// it was the time that ran out (what came is kept either way: a byte lost would swap I and Q).
pub fn read_up_to(sock: &mut TcpStream, buf: &mut [u8], within: Duration) -> std::io::Result<(usize, bool)> {
    let until = Instant::now() + within;
    let mut n = 0;
    while n < buf.len() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok((n, true));
        }
        sock.set_read_timeout(Some(left))?;
        match sock.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if timed_out(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok((n, false))
}
