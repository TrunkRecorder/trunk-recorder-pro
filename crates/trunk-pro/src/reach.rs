//! Where the built-in interface can be reached this run, and how: from this
//! computer, the local network, a tailnet, Tailscale Serve or Funnel, a
//! Cloudflare Tunnel, or `server.hostname`. A direct way (an address of this
//! computer's) depends on the address the recorder listens on (`--bind`,
//! else `server.bind`); a tunnel or proxy running here reaches it on
//! localhost whatever that is. Sent with the platform figures
//! (`platform.reach`): Setup → Access and the Interfaces page show it.
//!
//! What's found, not what's configured: the computer's addresses (as the
//! platform sampler reads them), `tailscale status` and `tailscale serve
//! status`, and cloudflared's metrics server (its `/config` ingress and
//! `/quicktunnel`), which listens on localhost:20241–20245 unless told
//! otherwise. Each tunnel's rule counts only when it leads to this port on
//! this computer.

use std::net::IpAddr;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use trunk_app::config::Server;

/// How often Tailscale and cloudflared are asked again.
pub const LOOK_EVERY: Duration = Duration::from_secs(60);
/// The ports cloudflared's metrics server takes, the first free one, when none is given.
const CLOUDFLARED_METRICS: std::ops::RangeInclusive<u16> = 20241..=20245;

/// Which computers an address listened on lets connect directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Listen {
    /// The loopback address: programs on this computer alone (a tunnel or proxy here among them).
    ThisComputer,
    /// Every address (`0.0.0.0`, `::`): any device that reaches one of this computer's.
    Every,
    /// One of this computer's addresses: devices that reach that one.
    One,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bind {
    pub address: String,
    pub listen: Listen,
}

impl Bind {
    pub fn of(address: &str) -> Bind {
        let listen = match address.trim().parse::<IpAddr>() {
            Ok(ip) if ip.is_loopback() => Listen::ThisComputer,
            Ok(ip) if ip.is_unspecified() => Listen::Every,
            // ("localhost", as a name.)
            _ if address.trim().eq_ignore_ascii_case("localhost") => Listen::ThisComputer,
            _ => Listen::One,
        };
        Bind { address: address.trim().to_string(), listen }
    }
}

/// The way a [`Route`] gets here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Via {
    /// A browser on this computer.
    Local,
    /// One of this computer's addresses on a network it's on (`iface`).
    Network,
    /// Its address on a tailnet: the devices on it.
    Tailnet,
    /// Tailscale Serve: the tailnet's devices, at its HTTPS name.
    TailscaleServe,
    /// Tailscale Funnel: anyone on the internet.
    Funnel,
    /// A Cloudflare Tunnel's hostname: anyone on the internet, unless Cloudflare Access is in front.
    Cloudflare,
    /// `server.hostname`, reached however that name is (nothing found for it here).
    Hostname,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Route {
    pub via: Via,
    pub url: String,
    /// The network interface its address is on (`en0`, `tailscale0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iface: Option<String>,
    /// From anywhere on the internet, not only a network of yours.
    pub public: bool,
    /// Gone when the recorder listens on this computer only.
    pub direct: bool,
}

/// Where the built-in interface can be reached this run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reach {
    /// The address listened on this run.
    pub now: Bind,
    /// `server.bind` as saved: what the next start listens on, unless `--bind` is given then.
    pub saved: Bind,
    /// This run's address is `--bind`'s, on the command line.
    pub flag: bool,
    pub routes: Vec<Route>,
}

/// What Tailscale and cloudflared said, of the ways that lead to this port.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// This computer's MagicDNS name, no final dot (Tailscale running, MagicDNS on).
    pub tailnet_name: Option<String>,
    /// Tailscale Serve's URLs that lead here, and whether Funnel opens each to the internet.
    pub served: Vec<(String, bool)>,
    /// Cloudflare Tunnel URLs that lead here.
    pub tunnels: Vec<String>,
}

/// Tailscale's addresses: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64,
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// An address another device could use: not loopback, unspecified, link-local or multicast.
fn usable(ip: IpAddr) -> bool {
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && match ip {
            IpAddr::V4(v4) => !v4.is_link_local(),
            IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 != 0xfe80,
        }
}

/// Whether listening on `bind` takes connections to `ip` (`::` takes IPv4's too, dual-stack).
fn covers(bind: IpAddr, ip: IpAddr) -> bool {
    match bind {
        IpAddr::V4(b) if b.is_unspecified() => ip.is_ipv4(),
        IpAddr::V6(b) if b.is_unspecified() => true,
        b => b == ip,
    }
}

fn host_of(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        v4 => v4.to_string(),
    }
}

/// Where the built-in interface on `port` can be reached, listening on `now`
/// (`flag`: `--bind`'s) with the config's `server`, this computer's `addrs`
/// (network interface, address) and what was `found`. `container`: inside
/// one, whose addresses no other device uses (its host's, at the port
/// published, are the ones).
pub fn reach(server: &Server, now: IpAddr, port: u16, flag: bool, addrs: &[(String, IpAddr)], found: &Found, container: bool) -> Reach {
    let scheme = server.scheme();
    let mut routes = Vec::new();
    if now.is_loopback() || now.is_unspecified() {
        routes.push(Route { via: Via::Local, url: format!("{scheme}://localhost:{port}"), iface: None, public: false, direct: false });
    }
    let reached: Vec<&(String, IpAddr)> = addrs.iter().filter(|(_, ip)| usable(*ip) && covers(now, *ip)).collect();
    // A network's: each interface's IPv4 addresses, else (none) its first IPv6 one; and one for the tailnet.
    let mut tailnet_done = false;
    for (iface, ip) in &reached {
        if tailnet(*ip) {
            if !tailnet_done {
                tailnet_done = true;
                let host = found.tailnet_name.clone().unwrap_or_else(|| host_of(*ip));
                routes.push(Route { via: Via::Tailnet, url: format!("{scheme}://{host}:{port}"), iface: Some(iface.clone()), public: false, direct: true });
            }
            continue;
        }
        let v4_here = reached.iter().any(|(i, a)| i == iface && a.is_ipv4() && !tailnet(*a));
        let first_v6 = reached.iter().find(|(i, a)| i == iface && a.is_ipv6() && !tailnet(*a)).is_some_and(|(_, a)| a == ip);
        if !container && (ip.is_ipv4() || (!v4_here && first_v6)) {
            routes.push(Route { via: Via::Network, url: format!("{scheme}://{}:{port}", host_of(*ip)), iface: Some(iface.clone()), public: false, direct: true });
        }
    }
    for (url, funnel) in &found.served {
        routes.push(Route { via: if *funnel { Via::Funnel } else { Via::TailscaleServe }, url: url.clone(), iface: None, public: *funnel, direct: false });
    }
    for url in &found.tunnels {
        routes.push(Route { via: Via::Cloudflare, url: url.clone(), iface: None, public: true, direct: false });
    }
    // `server.hostname`, unless it's one of those already.
    if let Some(url) = server.hostname_url(port, server.tls.is_some()) {
        let name = server.host().unwrap_or_default();
        if !routes.iter().any(|r| url_host(&r.url) == name) {
            routes.push(Route { via: Via::Hostname, url, iface: None, public: false, direct: false });
        }
    }
    Reach { now: Bind::of(&now.to_string()), saved: Bind::of(&server.bind), flag, routes }
}

/// A URL's host, lowercase.
fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if host.starts_with('[') { host.split_inclusive(']').next().unwrap_or(host) } else { host.rsplit_once(':').map_or(host, |(h, _)| h) };
    host.to_ascii_lowercase()
}

/// Whether a proxy's target (`http://127.0.0.1:8080`, `https+insecure://localhost:8443`,
/// `localhost:8080`, `8080`) is `port` on this computer.
fn leads_here(target: &str, port: u16) -> bool {
    let t = target.trim();
    if let Ok(p) = t.parse::<u16>() {
        return p == port;
    }
    let (scheme, rest) = t.split_once("://").unwrap_or(("http", t));
    let hostport = rest.split('/').next().unwrap_or("");
    let (host, p) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.ends_with(']') => (h, p.parse::<u16>().ok()),
        _ => (hostport, None),
    };
    let p = p.unwrap_or(if scheme.starts_with("https") { 443 } else { 80 });
    let local = host.eq_ignore_ascii_case("localhost") || host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    local && p == port
}

/// From `tailscale status --json`: this computer's MagicDNS name, while Tailscale runs.
pub fn tailnet_name(status: &Value) -> Option<String> {
    if status["BackendState"].as_str() != Some("Running") || status["CurrentTailnet"]["MagicDNSEnabled"].as_bool() == Some(false) {
        return None;
    }
    let name = status["Self"]["DNSName"].as_str()?.trim_end_matches('.').to_ascii_lowercase();
    (!name.is_empty()).then_some(name)
}

/// From `tailscale serve status --json`: the URLs whose handler proxies to
/// `port` here, and whether Funnel opens each to the internet.
pub fn served(serve: &Value, port: u16) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let Some(web) = serve["Web"].as_object() else { return out };
    for (hostport, site) in web {
        let (host, p) = hostport.rsplit_once(':').unwrap_or((hostport, "443"));
        let https = serve["TCP"][p]["HTTP"].as_bool() != Some(true);
        let base = match (https, p) {
            (true, "443") | (false, "80") => format!("{}://{host}", if https { "https" } else { "http" }),
            _ => format!("{}://{host}:{p}", if https { "https" } else { "http" }),
        };
        let funnel = serve["AllowFunnel"][hostport.as_str()].as_bool() == Some(true);
        for (path, h) in site["Handlers"].as_object().into_iter().flatten() {
            if h["Proxy"].as_str().is_some_and(|t| leads_here(t, port)) {
                out.push((format!("{base}{}", if path == "/" { "" } else { path }), funnel));
            }
        }
    }
    out.sort();
    out
}

/// From cloudflared's `/config` (and `/quicktunnel`'s name): the URLs whose
/// ingress rule leads to `port` here. A rule with no hostname is the quick
/// tunnel's, when there is one.
pub fn tunnels(config: &Value, quick: Option<&str>, port: u16) -> Vec<String> {
    let mut out = Vec::new();
    for rule in config["config"]["ingress"].as_array().into_iter().flatten() {
        if !rule["service"].as_str().is_some_and(|s| leads_here(s, port)) {
            continue;
        }
        let host = rule["hostname"].as_str().filter(|h| !h.is_empty()).or(quick.filter(|q| !q.is_empty()));
        if let Some(h) = host {
            let path = rule["path"].as_str().filter(|p| !p.is_empty() && *p != "/").unwrap_or("");
            out.push(format!("https://{h}{path}"));
        }
    }
    out.dedup();
    out
}

/// Ask Tailscale (when this computer has a tailnet address) and cloudflared
/// what leads to `port`. Blocks: run off the monitor's thread.
pub fn look(port: u16, on_tailnet: bool) -> Found {
    let mut found = Found::default();
    if on_tailnet {
        if let Some(status) = tailscale(&["status", "--json"]) {
            found.tailnet_name = tailnet_name(&status);
        }
        if let Some(serve) = tailscale(&["serve", "status", "--json"]) {
            found.served = served(&serve, port);
        }
    }
    for m in CLOUDFLARED_METRICS {
        let get = |path: &str| crate::store::get(&format!("http://127.0.0.1:{m}{path}"), 1 << 20, Duration::from_secs(2)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        // (Something else on the port answers /config with no ingress: nothing.)
        if let Some(config) = get("/config") {
            let quick = get("/quicktunnel");
            found.tunnels.extend(tunnels(&config, quick.as_ref().and_then(|q| q["hostname"].as_str()), port));
        }
    }
    found
}

/// `tailscale <args>`'s JSON: the CLI on the path, else where its app puts it.
fn tailscale(args: &[&str]) -> Option<Value> {
    let exes: &[&str] = if cfg!(target_os = "macos") {
        &["tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"]
    } else if cfg!(windows) {
        &["tailscale", r"C:\Program Files\Tailscale\tailscale.exe"]
    } else {
        &["tailscale"]
    };
    exes.iter().find_map(|exe| {
        let o = Command::new(exe).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        o.status.success().then(|| serde_json::from_slice(&o.stdout).ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn addrs() -> Vec<(String, IpAddr)> {
        [("lo0", "127.0.0.1"), ("en0", "192.168.1.100"), ("en0", "fe80::1"), ("en0", "2601::5"), ("en1", "fd00::7"), ("utun4", "100.103.2.79"), ("utun4", "fd7a:115c:a1e0::ca01:2a2"), ("en2", "169.254.3.3")]
            .iter()
            .map(|(i, a)| (i.to_string(), ip(a)))
            .collect()
    }

    fn vias(r: &Reach) -> Vec<(Via, &str)> {
        r.routes.iter().map(|r| (r.via, r.url.as_str())).collect()
    }

    #[test]
    fn binds_say_who_connects() {
        assert_eq!(["127.0.0.1", "::1", "localhost", "0.0.0.0", "::", "192.168.1.100"].map(|a| Bind::of(a).listen), [Listen::ThisComputer, Listen::ThisComputer, Listen::ThisComputer, Listen::Every, Listen::Every, Listen::One]);
        assert!(tailnet(ip("100.64.0.1")) && tailnet(ip("100.127.255.255")) && !tailnet(ip("100.128.0.1")) && !tailnet(ip("100.63.0.1")));
        assert!(tailnet(ip("fd7a:115c:a1e0::1")) && !tailnet(ip("fd7a:115c:a1e1::1")));
    }

    /// Listening on every address: this computer, each network's IPv4 address (an
    /// interface without one, its IPv6), and the tailnet once, by its name; not
    /// loopback or link-local. On localhost only this computer, and the tunnels.
    #[test]
    fn every_address_lists_the_networks() {
        let s = Server { port: 8080, ..Default::default() };
        let found = Found { tailnet_name: Some("radio.tail12cc5.ts.net".into()), ..Default::default() };
        let r = reach(&s, ip("0.0.0.0"), 8080, true, &addrs(), &found, false);
        assert_eq!(vias(&r), [(Via::Local, "http://localhost:8080"), (Via::Network, "http://192.168.1.100:8080"), (Via::Tailnet, "http://radio.tail12cc5.ts.net:8080")]);
        assert_eq!((r.now.listen, r.saved.listen, r.flag), (Listen::Every, Listen::ThisComputer, true));
        let v6 = reach(&s, ip("::"), 8080, false, &addrs(), &Found::default(), false);
        assert_eq!(vias(&v6)[1..], [(Via::Network, "http://192.168.1.100:8080"), (Via::Network, "http://[fd00::7]:8080"), (Via::Tailnet, "http://100.103.2.79:8080")]);
        let local = reach(&s, ip("127.0.0.1"), 8080, false, &addrs(), &found, false);
        assert_eq!(vias(&local), [(Via::Local, "http://localhost:8080")]);
        let one = reach(&s, ip("192.168.1.100"), 8080, false, &addrs(), &found, false);
        assert_eq!(vias(&one), [(Via::Network, "http://192.168.1.100:8080")], "one address: not localhost");
        let docker = reach(&s, ip("0.0.0.0"), 8080, true, &[("eth0".into(), ip("172.17.0.2"))], &Found::default(), true);
        assert_eq!(vias(&docker), [(Via::Local, "http://localhost:8080")], "a container's own address: no other device uses it");
    }

    /// Tunnels and the host name, whatever's listened on; `server.hostname` once.
    #[test]
    fn tunnels_and_the_host_name() {
        let s = Server { port: 8080, hostname: "radio.tail12cc5.ts.net".into(), ..Default::default() };
        let found = Found { served: vec![("https://radio.tail12cc5.ts.net".into(), true)], tunnels: vec!["https://radio.example.com".into()], ..Default::default() };
        let r = reach(&s, ip("127.0.0.1"), 8080, false, &addrs(), &found, false);
        assert_eq!(vias(&r), [(Via::Local, "http://localhost:8080"), (Via::Funnel, "https://radio.tail12cc5.ts.net"), (Via::Cloudflare, "https://radio.example.com")]);
        assert!(r.routes[1].public && !r.routes[1].direct);
        let named = Server { hostname: "radio.lan:9443".into(), ..s };
        assert_eq!(vias(&reach(&named, ip("127.0.0.1"), 8080, false, &[], &Found::default(), false))[1], (Via::Hostname, "https://radio.lan:9443"));
    }

    #[test]
    fn proxy_targets_here() {
        for t in ["http://127.0.0.1:8080", "http://localhost:8080/", "localhost:8080", "8080", "http://[::1]:8080"] {
            assert!(leads_here(t, 8080), "{t}");
        }
        for t in ["http://127.0.0.1:8081", "http://192.168.1.5:8080", "http://localhost", "https://localhost:8443", "http_status:404"] {
            assert!(!leads_here(t, 8080), "{t}");
        }
        assert!(leads_here("https+insecure://localhost:8443", 8443));
    }

    /// `tailscale status --json` and `tailscale serve status --json`, as they print.
    #[test]
    fn tailscale_says() {
        let status = json!({ "BackendState": "Running", "Self": { "DNSName": "Radio.tail12cc5.ts.net.", "TailscaleIPs": ["100.103.2.79"] }, "CurrentTailnet": { "MagicDNSEnabled": true } });
        assert_eq!(tailnet_name(&status).as_deref(), Some("radio.tail12cc5.ts.net"));
        assert_eq!(tailnet_name(&json!({ "BackendState": "Stopped", "Self": { "DNSName": "a.ts.net." } })), None);
        let serve = json!({
            "TCP": { "443": { "HTTPS": true }, "8443": { "HTTPS": true }, "80": { "HTTP": true } },
            "Web": {
                "radio.ts.net:443": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:8080" } } },
                "radio.ts.net:8443": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:9000" } } },
                "radio.ts.net:80": { "Handlers": { "/trp": { "Proxy": "localhost:8080" } } }
            },
            "AllowFunnel": { "radio.ts.net:443": true }
        });
        assert_eq!(served(&serve, 8080), [("http://radio.ts.net/trp".to_string(), false), ("https://radio.ts.net".to_string(), true)]);
        assert!(served(&json!({}), 8080).is_empty(), "no serve config");
    }

    /// cloudflared's `/config`: a token tunnel's and a config file's look alike.
    #[test]
    fn cloudflared_says() {
        let config = json!({ "version": 3, "config": { "ingress": [
            { "hostname": "radio.example.com", "service": "http://localhost:8080" },
            { "hostname": "other.example.com", "service": "http://localhost:3000" },
            { "service": "http_status:404" }
        ] } });
        assert_eq!(tunnels(&config, None, 8080), ["https://radio.example.com"]);
        let quick = json!({ "version": -1, "config": { "ingress": [{ "service": "http://localhost:8080" }] } });
        assert_eq!(tunnels(&quick, Some("xyz.trycloudflare.com"), 8080), ["https://xyz.trycloudflare.com"]);
        assert!(tunnels(&quick, Some(""), 8080).is_empty(), "no name: nothing to browse to");
    }
}
