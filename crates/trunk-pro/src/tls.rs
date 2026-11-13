//! HTTPS (`server.tls`): every interface's port, the built-in one's too,
//! served over TLS with the certificate and key the config names (PEM
//! files: `tailscale cert`'s, Let's Encrypt's, mkcert's). Read as the app
//! starts (a bad pair stops it: no quiet fall back to plain HTTP); a renewed
//! pair is picked up without a restart ([`Certs`]).
//!
//! [`TlsListener`] is what `axum::serve` accepts connections from: each
//! connection's handshake runs on its own, under [`HANDSHAKE`], so a stalled
//! one holds nothing up, and one that fails is only logged (at debug:
//! scanners). A browser or program speaking plain HTTP to the port gets
//! `GET /api/version` answered as ever (a second launch asks it) and a
//! redirect to https:// for anything else ([`plain`]).

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::rustls::crypto::{ring, CryptoProvider};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
use tokio_rustls::rustls::sign::CertifiedKey;
use tokio_rustls::rustls::{Error as TlsError, InconsistentKeys, ServerConfig};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

use crate::config;

/// How long a connection has to finish its handshake (or, speaking plain
/// HTTP, to send its request).
const HANDSHAKE: Duration = Duration::from_secs(10);

/// How often the certificate's files are looked at for a renewed pair.
const RENEWAL_CHECK: Duration = Duration::from_secs(60);

/// What a plain HTTP request may be, its line and headers: more is no request of ours.
const PLAIN_MAX: usize = 8 * 1024;

/// How this recorder serves HTTPS: the TLS settings every port's listener shares.
#[derive(Clone)]
pub struct Tls {
    config: Arc<ServerConfig>,
}

impl Tls {
    /// HTTPS with `t`'s certificate and key (the config at `config_path`), or why not.
    pub fn new(t: &config::Tls, config_path: &Path) -> Result<Tls, String> {
        Self::checking_every(t, config_path, RENEWAL_CHECK)
    }

    fn checking_every(t: &config::Tls, config_path: &Path, every: Duration) -> Result<Tls, String> {
        let (cert, key, now) = read(t, config_path)?;
        let seen = Seen { at: Instant::now(), stamps: stamps(&cert, &key), warned: false };
        let certs = Arc::new(Certs { cert, key, every, now: RwLock::new(now), seen: Mutex::new(seen) });
        let mut config = ServerConfig::builder_with_provider(Arc::new(provider())).with_safe_default_protocol_versions().map_err(|e| e.to_string())?.with_no_client_auth().with_cert_resolver(certs);
        // (axum serves HTTP/1.1; its WebSockets are HTTP/1.1's.)
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Tls { config: Arc::new(config) })
    }
}

/// Why `server.tls` can't be served as the config has it, if it can't (its
/// files missing or unreadable, not PEM, or a key that isn't the certificate's).
pub fn problem(cfg: &config::Config, config_path: &Path) -> Option<String> {
    read(cfg.server.tls.as_ref()?, config_path).err().map(|e| format!("HTTPS: {e}"))
}

fn provider() -> CryptoProvider {
    ring::default_provider()
}

/// `t`'s files (for the config at `config_path`) and what's in them.
fn read(t: &config::Tls, config_path: &Path) -> Result<(PathBuf, PathBuf, Arc<CertifiedKey>), String> {
    if t.cert.trim().is_empty() || t.key.trim().is_empty() {
        return Err("it needs a certificate file and its key file (server.tls cert and key).".into());
    }
    let (cert, key) = t.paths(config_path);
    let pair = load(&cert, &key)?;
    Ok((cert, key, pair))
}

/// The certificate chain at `cert` and its key at `key`, checked to belong together.
fn load(cert: &Path, key: &Path) -> Result<Arc<CertifiedKey>, String> {
    let read = |p: &Path, what: &str| std::fs::read(p).map_err(|e| format!("the {what} {}: {e}", p.display()));
    let (cert_pem, key_pem) = (read(cert, "certificate")?, read(key, "key")?);
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_pem).collect::<Result<_, _>>().map_err(|e| format!("{}: {e}", cert.display()))?;
    if chain.is_empty() {
        return Err(format!("{} has no certificate in it (a PEM file: -----BEGIN CERTIFICATE-----).", cert.display()));
    }
    let private = PrivateKeyDer::from_pem_slice(&key_pem).map_err(|_| format!("{} has no private key in it (a PEM file: -----BEGIN PRIVATE KEY-----).", key.display()))?;
    CertifiedKey::from_der(chain, private, &provider()).map(Arc::new).map_err(|e| match e {
        TlsError::InconsistentKeys(InconsistentKeys::KeyMismatch) => format!("the key {} isn't the certificate {}'s.", key.display(), cert.display()),
        e => format!("{} with {}: {e}", cert.display(), key.display()),
    })
}

/// When the certificate's and key's files were last changed.
type Stamps = (Option<SystemTime>, Option<SystemTime>);

fn stamps(cert: &Path, key: &Path) -> Stamps {
    let at = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    (at(cert), at(key))
}

/// The certificate handshakes are answered with: the pair as last read, read
/// again once its files change (looked at every `every`, as handshakes come).
/// A new pair that won't load is told once, and the old one kept meanwhile.
#[derive(Debug)]
struct Certs {
    cert: PathBuf,
    key: PathBuf,
    every: Duration,
    now: RwLock<Arc<CertifiedKey>>,
    seen: Mutex<Seen>,
}

#[derive(Debug)]
struct Seen {
    at: Instant,
    stamps: Stamps,
    warned: bool,
}

impl Certs {
    fn renew(&self) {
        let mut seen = self.seen.lock().unwrap();
        if seen.at.elapsed() < self.every {
            return;
        }
        seen.at = Instant::now();
        let stamps = stamps(&self.cert, &self.key);
        if stamps == seen.stamps {
            return;
        }
        match load(&self.cert, &self.key) {
            Ok(k) => {
                *self.now.write().unwrap() = k;
                *seen = Seen { stamps, warned: false, ..*seen };
                log::info!("HTTPS: {} read again (renewed)", self.cert.display());
            }
            // (Its stamps aren't taken: looked at again next time, as a renewal half written will be whole by then.)
            Err(e) if !seen.warned => {
                log::warn!("HTTPS: {e} Still serving the certificate read before.");
                seen.warned = true;
            }
            Err(_) => {}
        }
    }
}

impl ResolvesServerCert for Certs {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.renew();
        Some(self.now.read().unwrap().clone())
    }
}

/// Serve `app` on `listener` until `shutdown`: over TLS with `tls`, else
/// plain HTTP. Each request carries its connection's [`Peer`].
pub async fn serve(listener: TcpListener, tls: Option<&Tls>, app: axum::Router, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> io::Result<()> {
    let app = app.into_make_service_with_connect_info::<Peer>();
    match tls {
        Some(tls) => axum::serve(TlsListener::new(listener, tls)?, app).with_graceful_shutdown(shutdown).await,
        None => axum::serve(listener, app).with_graceful_shutdown(shutdown).await,
    }
}

/// Where a connection came from (its TCP peer's address), over HTTPS or plain
/// HTTP: in each request's extensions, as `ConnectInfo<Peer>`.
#[derive(Clone, Copy, Debug)]
pub struct Peer(pub SocketAddr);

impl Connected<IncomingStream<'_, TcpListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Peer {
        Peer(*stream.remote_addr())
    }
}

impl Connected<IncomingStream<'_, TlsListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, TlsListener>) -> Peer {
        Peer(*stream.remote_addr())
    }
}

/// The connections that finished their handshake, as `axum::serve` takes
/// them. Its port is listened on until it's dropped.
pub struct TlsListener {
    done: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    addr: SocketAddr,
}

impl TlsListener {
    pub fn new(listener: TcpListener, tls: &Tls) -> io::Result<TlsListener> {
        let addr = listener.local_addr()?;
        let (tx, done) = mpsc::channel(32);
        tokio::spawn(accept(listener, TlsAcceptor::from(tls.config.clone()), tx));
        Ok(TlsListener { done, addr })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.done.recv().await {
            Some(c) => c,
            // (Not while this is: the accepting ends when it's dropped.)
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

/// Take `listener`'s connections, each shaking hands on its own, until the
/// [`TlsListener`] is gone (then the port is let go of).
async fn accept(mut listener: TcpListener, acceptor: TlsAcceptor, tx: mpsc::Sender<(TlsStream<TcpStream>, SocketAddr)>) {
    loop {
        // (axum's accept: it retries what fails, as axum does for plain HTTP.)
        let (tcp, peer) = tokio::select! {
            _ = tx.closed() => return,
            c = axum::serve::Listener::accept(&mut listener) => c,
        };
        let (acceptor, tx) = (acceptor.clone(), tx.clone());
        tokio::spawn(async move {
            match tokio::time::timeout(HANDSHAKE, handshake(tcp, acceptor)).await {
                Ok(Ok(Some(tls))) => {
                    let _ = tx.send((tls, peer)).await;
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => log::debug!("HTTPS: {peer}: {e}"),
                Err(_) => log::debug!("HTTPS: {peer}: no handshake in {} s", HANDSHAKE.as_secs()),
            }
        });
    }
}

/// A connection's handshake; one speaking plain HTTP is answered so ([`plain`]) and closed: None.
async fn handshake(tcp: TcpStream, acceptor: TlsAcceptor) -> io::Result<Option<TlsStream<TcpStream>>> {
    let mut first = [0u8; 1];
    if tcp.peek(&mut first).await? == 0 {
        return Ok(None);
    }
    // (A TLS handshake record starts 0x16.)
    if first[0] != 0x16 {
        plain(tcp).await?;
        return Ok(None);
    }
    acceptor.accept(tcp).await.map(Some)
}

/// Plain HTTP on an HTTPS port: `GET /api/version` answered as ever (how a
/// second launch, or a script, finds the recorder), anything else sent to
/// the same address over https:// (308, so a POST stays one).
async fn plain(mut tcp: TcpStream) -> io::Result<()> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tcp.read(&mut buf).await?;
        if n == 0 || head.len() + n > PLAIN_MAX {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let mut line = lines.next().unwrap_or("").split(' ');
    let (method, target) = (line.next().unwrap_or(""), line.next().unwrap_or(""));
    let host = lines.take_while(|l| !l.is_empty()).find_map(|l| l.split_once(':').filter(|(n, _)| n.trim().eq_ignore_ascii_case("host")).map(|(_, v)| v.trim()));
    let answer = |status: &str, headers: &str, body: &str| format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let res = if method == "GET" && target.split('?').next() == Some("/api/version") {
        answer("200 OK", "Content-Type: application/json\r\n", &crate::server::version().to_string())
    } else {
        match host.filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._:[]".contains(&b))) {
            Some(host) => {
                let path = if target.starts_with('/') && target.bytes().all(|b| b.is_ascii_graphic()) { target } else { "/" };
                let to = format!("https://{host}{path}");
                answer("308 Permanent Redirect", &format!("Location: {to}\r\nContent-Type: text/plain; charset=utf-8\r\n"), &format!("This recorder serves HTTPS: {to}\n"))
            }
            None => answer("400 Bad Request", "Content-Type: text/plain; charset=utf-8\r\n", "This recorder serves HTTPS.\n"),
        }
    };
    tcp.write_all(res.as_bytes()).await?;
    tcp.shutdown().await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio_rustls::rustls::{ClientConfig, RootCertStore};
    use tokio_rustls::TlsConnector;

    /// The test fixtures' certificate `n` ("a" or "b": for localhost and 127.0.0.1) and its key.
    pub(crate) fn fixture(n: &str) -> (PathBuf, PathBuf) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tls");
        (dir.join(format!("{n}.crt")), dir.join(format!("{n}.key")))
    }

    pub(crate) fn config_of(cert: &Path, key: &Path) -> config::Tls {
        config::Tls { cert: cert.display().to_string(), key: key.display().to_string() }
    }

    /// A TLS client trusting the fixtures' certificates.
    fn client() -> TlsConnector {
        let mut roots = RootCertStore::empty();
        for n in ["a", "b"] {
            roots.add(CertificateDer::from_pem_file(fixture(n).0).unwrap()).unwrap();
        }
        let c = ClientConfig::builder_with_provider(Arc::new(provider())).with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth();
        TlsConnector::from(Arc::new(c))
    }

    /// `request` sent over HTTPS to `addr` (as localhost): the answer, and the certificate it came with.
    pub(crate) async fn https(addr: SocketAddr, request: &str) -> (String, Vec<u8>) {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut s = client().connect("localhost".try_into().unwrap(), tcp).await.unwrap();
        let cert = s.get_ref().1.peer_certificates().unwrap()[0].to_vec();
        s.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out).await;
        (String::from_utf8_lossy(&out).into_owned(), cert)
    }

    /// `request` sent over plain HTTP to `addr`: the answer.
    pub(crate) async fn http(addr: SocketAddr, request: &str) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        out
    }

    fn der(cert: &Path) -> Vec<u8> {
        CertificateDer::from_pem_file(cert).unwrap().to_vec()
    }

    async fn serving(tls: &Tls) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "hello" }));
        let tls = tls.clone();
        tokio::spawn(async move { serve(listener, Some(&tls), app, std::future::pending()).await });
        addr
    }

    #[test]
    fn a_pair_that_isnt_one_is_refused_and_said_why() {
        let (a, a_key) = fixture("a");
        let (b, b_key) = fixture("b");
        let dir = Path::new("/");
        assert!(Tls::new(&config_of(&a, &a_key), dir).is_ok());
        let no = |cert: &Path, key: &Path| Tls::new(&config_of(cert, key), dir).err().unwrap();
        assert!(no(&a, &b_key).contains("isn't the certificate"), "{}", no(&a, &b_key));
        assert!(no(&a, Path::new("/nowhere.key")).contains("the key /nowhere.key: "), "{}", no(&a, Path::new("/nowhere.key")));
        assert!(no(&a_key, &a_key).contains("has no certificate in it"), "{}", no(&a_key, &a_key));
        assert!(no(&b, &b).contains("has no private key in it"), "{}", no(&b, &b));
        assert!(Tls::new(&config::Tls::default(), dir).err().unwrap().contains("needs a certificate file"));
        // Relative: from the config's folder.
        let fixtures = a.parent().unwrap();
        assert!(Tls::new(&config::Tls { cert: "a.crt".into(), key: "a.key".into() }, &fixtures.join("config.json")).is_ok());
        let mut cfg = config::Config::default();
        assert_eq!(problem(&cfg, dir), None, "off: nothing to check");
        cfg.server.tls = Some(config_of(&a, &b_key));
        assert!(problem(&cfg, dir).unwrap().starts_with("HTTPS: the key"));
    }

    /// HTTPS with the certificate; plain HTTP to the same port gets the
    /// version and a redirect; a client that never shakes hands holds up nobody.
    #[tokio::test]
    async fn https_and_plain_http_on_one_port() {
        let (a, a_key) = fixture("a");
        let tls = Tls::new(&config_of(&a, &a_key), Path::new("/")).unwrap();
        let addr = serving(&tls).await;
        // Stalled: connected, and silent (and another that sent half a request).
        let _stalled = TcpStream::connect(addr).await.unwrap();
        let mut half = TcpStream::connect(addr).await.unwrap();
        half.write_all(b"GET / HT").await.unwrap();
        let (res, cert) = tokio::time::timeout(Duration::from_secs(5), https(addr, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")).await.unwrap();
        assert!(res.starts_with("HTTP/1.1 200") && res.ends_with("hello"), "{res}");
        assert_eq!(cert, der(&a));
        let res = http(addr, "GET /api/version HTTP/1.0\r\nHost: localhost\r\n\r\n").await;
        assert!(res.starts_with("HTTP/1.1 200 OK") && res.contains(&format!("\"app\":\"{}\"", crate::server::APP_ID)), "{res}");
        let res = http(addr, &format!("GET /?x=1 HTTP/1.1\r\nHost: localhost:{}\r\n\r\n", addr.port())).await;
        assert!(res.starts_with("HTTP/1.1 308") && res.contains(&format!("\r\nLocation: https://localhost:{}/?x=1\r\n", addr.port())), "{res}");
        let res = http(addr, "POST /api/login HTTP/1.1\r\nHost: radiobox.example\r\nContent-Length: 0\r\n\r\n").await;
        assert!(res.contains("\r\nLocation: https://radiobox.example/api/login\r\n"), "{res}");
        assert!(http(addr, "GET / HTTP/1.1\r\nHost: a b\r\n\r\n").await.starts_with("HTTP/1.1 400"), "no Location from a Host that's no host");
    }

    /// A renewed pair is served once its files change; one that won't load
    /// leaves the one before served.
    #[tokio::test]
    async fn a_renewed_certificate_is_served_without_a_restart() {
        let dir = crate::db::TestDir::new("tls-renew");
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        let put = |n: &str, later: u64| {
            let (c, k) = fixture(n);
            for (from, to) in [(c, &cert), (k, &key)] {
                std::fs::copy(from, to).unwrap();
                let f = std::fs::File::options().write(true).open(to).unwrap();
                f.set_modified(SystemTime::now() + Duration::from_secs(later)).unwrap();
            }
        };
        put("a", 0);
        let tls = Tls::checking_every(&config_of(&cert, &key), &dir.join("config.json"), Duration::ZERO).unwrap();
        let addr = serving(&tls).await;
        let ask = || https(addr, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert_eq!(ask().await.1, der(&fixture("a").0));
        put("b", 10);
        assert_eq!(ask().await.1, der(&fixture("b").0), "renewed");
        std::fs::write(&key, "not a key").unwrap();
        let f = std::fs::File::options().write(true).open(&key).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(20)).unwrap();
        assert_eq!(ask().await.1, der(&fixture("b").0), "a renewal gone wrong: the one before, still");
    }
}
