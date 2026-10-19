//! A listener for each interface to be served, kept as the config says:
//! [`reconcile`] starts the new ones, stops those turned off or removed, and
//! moves those whose port changed. Each serves its folder and the recorder's
//! API ([`crate::server::interface_app`]). Stopping one closes its pages'
//! connections too.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::runtime::{publish, Ctx};

/// An interface being served.
pub struct Serving {
    pub port: u16,
    /// Turned true (or dropped) to stop it.
    stop: watch::Sender<bool>,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Serve what the config says now, then tell the browsers how that went
/// (an `interfaces` message). Called at start and whenever the interfaces
/// may have changed.
pub async fn reconcile(ctx: Arc<Ctx>) {
    // One at a time: two at once could both start the same one.
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    let cfg = ctx.config.lock().unwrap().clone();
    let want: BTreeMap<String, u16> = ctx
        .interfaces
        .all(&cfg, &ctx.config_path)
        .into_iter()
        .filter(|(id, (i, _))| i.enabled && ctx.interfaces.problem(&cfg, id).is_none())
        .map(|(id, (i, _))| (id, i.port))
        .collect();
    ctx.interfaces.serving.lock().unwrap().retain(|id, s| {
        let keep = want.get(id) == Some(&s.port);
        if !keep {
            log::info!("Interface {id}: no longer served on port {}", s.port);
        }
        keep
    });
    let mut problems = BTreeMap::new();
    for (id, port) in want {
        if ctx.interfaces.serving.lock().unwrap().contains_key(&id) {
            continue;
        }
        let addr = SocketAddr::new(ctx.bind, port);
        match bind(addr).await {
            Ok(listener) => {
                let (stop, stopped) = watch::channel(false);
                log::info!("Interface {id}: http://{}:{port}/", if ctx.bind.is_unspecified() { "localhost".to_string() } else { ctx.bind.to_string() });
                tokio::spawn(run(ctx.clone(), id.clone(), listener, stopped));
                ctx.interfaces.serving.lock().unwrap().insert(id, Serving { port, stop });
            }
            Err(e) => {
                log::warn!("Interface {id}: can't serve it on port {port}: {e}");
                problems.insert(id, e);
            }
        }
    }
    *ctx.interfaces.problems.lock().unwrap() = problems;
    publish(&ctx.hub, ctx.interfaces.list(&cfg, &ctx.config_path));
}

/// Stop serving them all (the app is quitting).
pub fn stop_all(ctx: &Ctx) {
    ctx.interfaces.serving.lock().unwrap().clear();
}

/// Bind `addr`; a port just let go of may take a moment to be free again.
async fn bind(addr: SocketAddr) -> Result<std::net::TcpListener, String> {
    let mut tries = 0;
    loop {
        match std::net::TcpListener::bind(addr) {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && tries < 10 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Err(format!("Port {} is in use by another program.", addr.port())),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(format!("Port {} needs administrator rights here: pick one above 1023.", addr.port())),
            Err(e) => return Err(format!("Port {}: {e}", addr.port())),
        }
    }
}

async fn run(ctx: Arc<Ctx>, id: String, listener: std::net::TcpListener, stop: watch::Receiver<bool>) {
    let loopback = ctx.bind.is_loopback();
    let listener = match listener.set_nonblocking(true).and_then(|_| tokio::net::TcpListener::from_std(listener)) {
        Ok(l) => l,
        Err(e) => return log::warn!("Interface {id}: {e}"),
    };
    let app = crate::server::interface_app(ctx.clone(), id.clone(), stop.clone(), loopback);
    let mut until = stop;
    let shutdown = async move {
        let _ = until.wait_for(|stopped| *stopped).await;
    };
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
        log::warn!("Interface {id}: {e}");
    }
}
