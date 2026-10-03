//! Accounts: admins set up and run the recorder, viewers watch and listen.
//!
//! Kept in `accounts.json` beside the config (owner-only): users with salted
//! PBKDF2-HMAC-SHA256 password hashes, and login sessions by the SHA-256 of
//! their token. With no accounts, only this computer gets in, as an admin.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(not(test))]
const ITERATIONS: u32 = 600_000;
#[cfg(test)]
const ITERATIONS: u32 = 1_000;
/// A session unused this long ends.
const SESSION_IDLE_S: i64 = 30 * 24 * 3600;
/// How often a session's last use is written down.
const TOUCH_S: i64 = 300;
/// Failed logins from one address before it has to wait.
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
pub const MIN_PASSWORD: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Viewer,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Viewer => "viewer",
        }
    }
}

/// Who a request is from.
#[derive(Clone, Debug, PartialEq)]
pub struct Who {
    /// "" when there are no accounts (this computer, as an admin).
    pub user: String,
    pub role: Role,
    /// The SHA-256 of the session token (None: no accounts).
    pub session: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct User {
    name: String,
    role: Role,
    salt: String,
    hash: String,
    iterations: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Session {
    token_hash: String,
    user: String,
    created: i64,
    last_seen: i64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    users: Vec<User>,
    sessions: Vec<Session>,
}

pub struct Accounts {
    path: PathBuf,
    store: Mutex<Store>,
    /// The file's modification time when last read or written: another
    /// process (`trunk-pro account`) may change it.
    seen: Mutex<Option<std::time::SystemTime>>,
    /// The file exists but couldn't be read: nobody gets in until it's fixed.
    broken: Option<String>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

pub enum LoginError {
    Wrong,
    /// Too many failures from this address: seconds to wait.
    Wait(u64),
}

impl Accounts {
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name("accounts.json")
    }

    pub fn load(path: PathBuf) -> Accounts {
        let (store, broken) = match std::fs::read_to_string(&path) {
            Ok(t) => match serde_json::from_str::<Store>(&t) {
                Ok(s) => (s, None),
                Err(e) => (Store::default(), Some(format!("{} isn't readable ({e}); nobody can log in until it's fixed or removed", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Store::default(), None),
            Err(e) => (Store::default(), Some(format!("{} isn't readable ({e}); nobody can log in until it's fixed", path.display()))),
        };
        let seen = Mutex::new(modified(&path));
        Accounts { path, store: Mutex::new(store), seen, broken, failures: Mutex::new(HashMap::new()) }
    }

    /// The store, read again first if the file changed underneath us.
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        let mut s = self.store.lock().unwrap();
        let now = modified(&self.path);
        let mut seen = self.seen.lock().unwrap();
        if now != *seen && self.broken.is_none() {
            match std::fs::read_to_string(&self.path).map(|t| serde_json::from_str::<Store>(&t)) {
                Ok(Ok(fresh)) => *s = fresh,
                // Removed: no accounts.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => *s = Store::default(),
                // Half-written or damaged: keep what we have.
                _ => return s,
            }
            *seen = now;
        }
        s
    }

    /// Why nobody can log in, when the file is damaged.
    pub fn problem(&self) -> Option<&str> {
        self.broken.as_deref()
    }

    /// No accounts yet: this computer gets in as an admin, nobody else does.
    pub fn is_open(&self) -> bool {
        self.broken.is_none() && self.store().users.is_empty()
    }

    /// Who's at `peer` with session `token` (from the cookie).
    pub fn who(&self, peer: IpAddr, token: Option<&str>) -> Option<Who> {
        if self.is_open() {
            return peer.is_loopback().then(|| Who { user: String::new(), role: Role::Admin, session: None });
        }
        self.session(token?)
    }

    fn session(&self, token: &str) -> Option<Who> {
        let h = hex(&Sha256::digest(token.as_bytes()));
        let now = unix_now();
        let mut s = self.store();
        let i = s.sessions.iter().position(|x| x.token_hash == h)?;
        let Some(user) = s.users.iter().find(|u| u.name == s.sessions[i].user).cloned() else {
            s.sessions.remove(i);
            return None;
        };
        if now - s.sessions[i].last_seen > SESSION_IDLE_S {
            s.sessions.remove(i);
            let _ = self.save(&s);
            return None;
        }
        if now - s.sessions[i].last_seen > TOUCH_S {
            s.sessions[i].last_seen = now;
            let _ = self.save(&s);
        }
        Some(Who { user: user.name, role: user.role, session: Some(h) })
    }

    /// Whether the session `who` came with is still good (not logged out, the
    /// user not removed, the role unchanged).
    pub fn still(&self, who: &Who) -> bool {
        match &who.session {
            None => self.is_open(),
            Some(h) => {
                let s = self.store();
                s.sessions.iter().find(|x| &x.token_hash == h).and_then(|x| s.users.iter().find(|u| u.name == x.user)).is_some_and(|u| u.role == who.role)
            }
        }
    }

    /// Check a password (slow: PBKDF2) and start a session: its token.
    pub fn login(&self, name: &str, password: &str, peer: IpAddr) -> Result<(String, Who), LoginError> {
        {
            let mut f = self.failures.lock().unwrap();
            f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
            if let Some((n, t)) = f.get(&peer) {
                if *n >= MAX_FAILURES {
                    return Err(LoginError::Wait(FAILURE_WINDOW.saturating_sub(t.elapsed()).as_secs().max(1)));
                }
            }
        }
        let user = self.store().users.iter().find(|u| u.name == name).cloned();
        let ok = match &user {
            Some(u) => verify(password, u),
            None => {
                // The same work either way, so timing doesn't tell which names exist.
                let _ = pbkdf2_sha256(password.as_bytes(), b"no such user", ITERATIONS);
                false
            }
        };
        if !ok || self.broken.is_some() {
            let mut f = self.failures.lock().unwrap();
            let e = f.entry(peer).or_insert((0, Instant::now()));
            e.0 += 1;
            return Err(LoginError::Wrong);
        }
        self.failures.lock().unwrap().remove(&peer);
        let user = user.unwrap();
        let token = random_hex(32);
        let token_hash = hex(&Sha256::digest(token.as_bytes()));
        let now = unix_now();
        let mut s = self.store();
        s.sessions.retain(|x| now - x.last_seen <= SESSION_IDLE_S);
        s.sessions.push(Session { token_hash: token_hash.clone(), user: user.name.clone(), created: now, last_seen: now });
        let _ = self.save(&s);
        Ok((token, Who { user: user.name, role: user.role, session: Some(token_hash) }))
    }

    pub fn logout(&self, who: &Who) {
        if let Some(h) = &who.session {
            let mut s = self.store();
            s.sessions.retain(|x| &x.token_hash != h);
            let _ = self.save(&s);
        }
    }

    /// (name, role, sessions)
    pub fn list(&self) -> Vec<(String, Role, usize)> {
        let s = self.store();
        s.users.iter().map(|u| (u.name.clone(), u.role, s.sessions.iter().filter(|x| x.user == u.name).count())).collect()
    }

    /// Add an account (slow: PBKDF2).
    pub fn add(&self, name: &str, role: Role, password: &str) -> Result<(), String> {
        self.writable()?;
        check_name(name)?;
        check_password(password)?;
        let user = new_user(name, role, password);
        let mut s = self.store();
        if s.users.iter().any(|u| u.name == name) {
            return Err(format!("There's already an account named {name}."));
        }
        s.users.push(user);
        self.save(&s)
    }

    pub fn remove(&self, name: &str) -> Result<(), String> {
        self.writable()?;
        let mut s = self.store();
        let Some(u) = s.users.iter().find(|u| u.name == name) else { return Err(format!("No account named {name}.")) };
        if u.role == Role::Admin && s.users.iter().filter(|u| u.role == Role::Admin).count() == 1 && s.users.len() > 1 {
            return Err("That's the last admin account: make another admin first.".into());
        }
        s.users.retain(|u| u.name != name);
        s.sessions.retain(|x| x.user != name);
        self.save(&s)
    }

    pub fn set_role(&self, name: &str, role: Role) -> Result<(), String> {
        self.writable()?;
        let mut s = self.store();
        let admins = s.users.iter().filter(|u| u.role == Role::Admin).count();
        let Some(u) = s.users.iter_mut().find(|u| u.name == name) else { return Err(format!("No account named {name}.")) };
        if u.role == Role::Admin && role != Role::Admin && admins == 1 {
            return Err("That's the last admin account: make another admin first.".into());
        }
        u.role = role;
        self.save(&s)
    }

    /// A new password (slow: PBKDF2). The account's other sessions end; `keep`
    /// (the session that changed it) stays.
    pub fn set_password(&self, name: &str, password: &str, keep: Option<&str>) -> Result<(), String> {
        self.writable()?;
        check_password(password)?;
        let role = self.store().users.iter().find(|u| u.name == name).map(|u| u.role).ok_or_else(|| format!("No account named {name}."))?;
        let fresh = new_user(name, role, password);
        let mut s = self.store();
        if let Some(u) = s.users.iter_mut().find(|u| u.name == name) {
            *u = fresh;
        }
        s.sessions.retain(|x| x.user != name || Some(x.token_hash.as_str()) == keep);
        self.save(&s)
    }

    /// Check `name`'s password (slow).
    pub fn check(&self, name: &str, password: &str) -> bool {
        let u = self.store().users.iter().find(|u| u.name == name).cloned();
        u.is_some_and(|u| verify(password, &u))
    }

    fn writable(&self) -> Result<(), String> {
        match &self.broken {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn save(&self, s: &Store) -> Result<(), String> {
        let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
        write_private(&self.path, text.as_bytes()).map_err(|e| format!("Couldn't save {}: {e}", self.path.display()))?;
        *self.seen.lock().unwrap() = modified(&self.path);
        Ok(())
    }
}

const CLI_USAGE: &str = "\
usage: trunk-pro account <command> [--config file.json]
  list                         the accounts
  add <name> [--role admin|viewer]   a new account (admin by default); asks for its password
  remove <name>
  role <name> admin|viewer
  passwd <name>                a new password; ends the account's sessions
The recorder picks up changes at once; it needn't be restarted.";

/// `trunk-pro account …`
pub fn cli(a: &crate::Args) {
    let config_path = a.get("config").map(PathBuf::from).unwrap_or_else(|| crate::config::config_dir().join("config.json"));
    let accounts = Accounts::load(Accounts::path_for(&config_path));
    let arg = |i: usize| a.positional.get(i).map(|s| s.as_str());
    let r = match (arg(0), arg(1)) {
        (Some("list"), _) => {
            if let Some(p) = accounts.problem() {
                eprintln!("{p}");
            }
            let list = accounts.list();
            if list.is_empty() {
                println!("No accounts: only this computer can open the interface.");
            }
            for (name, role, sessions) in list {
                println!("{name:<24} {:<7} {sessions} session(s)", role.as_str());
            }
            Ok(())
        }
        (Some("add"), Some(name)) => match Role::parse(a.get("role").unwrap_or("admin")) {
            Some(role) => read_password(name).and_then(|p| accounts.add(name, role, &p)),
            None => Err("--role is admin or viewer".into()),
        },
        (Some("remove"), Some(name)) => accounts.remove(name),
        (Some("role"), Some(name)) => match arg(2).and_then(Role::parse) {
            Some(role) => accounts.set_role(name, role),
            None => Err("the role is admin or viewer".into()),
        },
        (Some("passwd"), Some(name)) => read_password(name).and_then(|p| accounts.set_password(name, &p, None)),
        _ => {
            eprintln!("{CLI_USAGE}");
            std::process::exit(2)
        }
    };
    match r {
        Ok(()) => {
            if arg(0) != Some("list") {
                println!("Saved to {}", Accounts::path_for(&config_path).display());
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}

/// A password from the terminal (not echoed), asked twice; or one line from stdin.
fn read_password(name: &str) -> Result<String, String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    let line = || -> Result<String, String> {
        let mut s = String::new();
        stdin.lock().read_line(&mut s).map_err(|e| e.to_string())?;
        Ok(s.trim_end_matches(['\r', '\n']).to_string())
    };
    if !stdin.is_terminal() {
        return line();
    }
    let echo = |on: bool| {
        #[cfg(unix)]
        let _ = std::process::Command::new("stty").arg(if on { "echo" } else { "-echo" }).stdin(std::process::Stdio::inherit()).status();
        #[cfg(not(unix))]
        let _ = on;
    };
    let ask = |prompt: &str| -> Result<String, String> {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        echo(false);
        let r = line();
        echo(true);
        println!();
        r
    };
    let p = ask(&format!("Password for {name}: "))?;
    if ask("Again: ")? != p {
        return Err("The passwords didn't match.".into());
    }
    Ok(p)
}

fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)) {
        return Err("A name is 1 to 32 letters, digits, dots, dashes, underscores or @.".into());
    }
    Ok(())
}

fn check_password(p: &str) -> Result<(), String> {
    if p.chars().count() < MIN_PASSWORD {
        return Err(format!("A password needs at least {MIN_PASSWORD} characters."));
    }
    Ok(())
}

fn new_user(name: &str, role: Role, password: &str) -> User {
    let salt = random_hex(16);
    let hash = hex(&pbkdf2_sha256(password.as_bytes(), salt.as_bytes(), ITERATIONS));
    User { name: name.to_string(), role, salt, hash, iterations: ITERATIONS }
}

fn verify(password: &str, u: &User) -> bool {
    let h = hex(&pbkdf2_sha256(password.as_bytes(), u.salt.as_bytes(), u.iterations.max(1)));
    eq_ct(h.as_bytes(), u.hash.as_bytes())
}

/// Write `path` readable by its owner only: a temporary file, synced, renamed over.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).expect("the system's random number source");
    hex(&b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn eq_ct(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// HMAC-SHA256's inner and outer hashes, keyed.
fn hmac_keyed(key: &[u8]) -> (Sha256, Sha256) {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let (mut inner, mut outer) = (Sha256::new(), Sha256::new());
    inner.update(k.map(|b| b ^ 0x36));
    outer.update(k.map(|b| b ^ 0x5c));
    (inner, outer)
}

fn hmac_with(keyed: &(Sha256, Sha256), msg: &[u8]) -> [u8; 32] {
    let mut i = keyed.0.clone();
    i.update(msg);
    let mut o = keyed.1.clone();
    o.update(i.finalize());
    o.finalize().into()
}

#[cfg(test)]
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    hmac_with(&hmac_keyed(key), msg)
}

/// PBKDF2-HMAC-SHA256, one 32-byte block.
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let keyed = hmac_keyed(password);
    let mut u = hmac_with(&keyed, &[salt, &1u32.to_be_bytes()].concat());
    let mut t = u;
    for _ in 1..iterations {
        u = hmac_with(&keyed, &u);
        for (a, b) in t.iter_mut().zip(u) {
            *a ^= b;
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_and_pbkdf2_vectors() {
        // RFC 4231 test case 2.
        assert_eq!(hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // A key longer than the block is hashed first (RFC 4231 test case 6).
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        // PBKDF2-HMAC-SHA256 (RFC 7914 / the widely published vectors).
        assert_eq!(hex(&pbkdf2_sha256(b"password", b"salt", 1)), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
        assert_eq!(hex(&pbkdf2_sha256(b"password", b"salt", 2)), "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43");
        assert_eq!(hex(&pbkdf2_sha256(b"password", b"salt", 4096)), "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a");
    }

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("trunk-pro-auth-{}-{}", std::process::id(), random_hex(4)));
        std::fs::create_dir_all(&d).unwrap();
        d.join("accounts.json")
    }

    const LOCAL: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    const REMOTE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 50));

    #[test]
    fn no_accounts_lets_only_this_computer_in() {
        let a = Accounts::load(temp());
        assert!(a.is_open());
        assert_eq!(a.who(LOCAL, None).map(|w| w.role), Some(Role::Admin));
        assert_eq!(a.who(REMOTE, None), None);
        a.add("nick", Role::Admin, "correct horse").unwrap();
        assert!(!a.is_open());
        assert_eq!(a.who(LOCAL, None), None, "with accounts, this computer logs in too");
    }

    #[test]
    fn sessions() {
        let path = temp();
        let a = Accounts::load(path.clone());
        a.add("admin", Role::Admin, "admin-pass").unwrap();
        a.add("watch", Role::Viewer, "viewer-pass").unwrap();
        assert!(matches!(a.login("watch", "wrong-pass", REMOTE), Err(LoginError::Wrong)));
        let (token, who) = a.login("watch", "viewer-pass", REMOTE).ok().unwrap();
        assert_eq!((who.user.as_str(), who.role), ("watch", Role::Viewer));
        assert_eq!(a.who(REMOTE, Some(&token)).map(|w| w.role), Some(Role::Viewer));
        assert_eq!(a.who(REMOTE, Some("not-a-token")), None);
        // Across a restart, and without the token in the file.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&token) && !text.contains("viewer-pass"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let b = Accounts::load(path.clone());
        assert!(b.who(REMOTE, Some(&token)).is_some());
        // A role change ends what the session could do.
        b.set_role("watch", Role::Admin).unwrap();
        assert!(!b.still(&who));
        b.logout(&b.who(REMOTE, Some(&token)).unwrap());
        assert_eq!(b.who(REMOTE, Some(&token)), None);
    }

    #[test]
    fn a_new_password_ends_other_sessions() {
        let a = Accounts::load(temp());
        a.add("nick", Role::Admin, "first-pass").unwrap();
        let (t1, w1) = a.login("nick", "first-pass", LOCAL).ok().unwrap();
        let (t2, _) = a.login("nick", "first-pass", REMOTE).ok().unwrap();
        a.set_password("nick", "second-pass", w1.session.as_deref()).unwrap();
        assert!(a.who(LOCAL, Some(&t1)).is_some());
        assert!(a.who(REMOTE, Some(&t2)).is_none());
        assert!(a.check("nick", "second-pass") && !a.check("nick", "first-pass"));
    }

    #[test]
    fn only_wrong_passwords_count_toward_the_limit() {
        let a = Accounts::load(temp());
        a.add("nick", Role::Admin, "the-password").unwrap();
        // Requests without a login (a page polling before anyone logs in) don't count.
        for _ in 0..50 {
            assert!(a.who(REMOTE, None).is_none());
        }
        for _ in 0..MAX_FAILURES {
            assert!(matches!(a.login("nick", "nope-nope", REMOTE), Err(LoginError::Wrong)));
        }
        assert!(matches!(a.login("nick", "the-password", REMOTE), Err(LoginError::Wait(_))));
        // Another address isn't held up.
        assert!(a.login("nick", "the-password", LOCAL).is_ok());
    }

    #[test]
    fn the_last_admin_stays() {
        let a = Accounts::load(temp());
        a.add("nick", Role::Admin, "the-password").unwrap();
        a.add("view", Role::Viewer, "the-password").unwrap();
        assert!(a.remove("nick").is_err());
        assert!(a.set_role("nick", Role::Viewer).is_err());
        assert!(a.add("bad name", Role::Viewer, "the-password").is_err());
        assert!(a.add("short", Role::Viewer, "short").is_err());
        a.remove("view").unwrap();
        // The only account left can go: back to this computer only.
        a.remove("nick").unwrap();
        assert!(a.is_open());
    }

    #[test]
    fn changes_from_another_process_are_picked_up() {
        let path = temp();
        let server = Accounts::load(path.clone());
        server.add("nick", Role::Admin, "the-password").unwrap();
        // `trunk-pro account add` in another process.
        std::thread::sleep(Duration::from_millis(20));
        Accounts::load(path.clone()).add("watch", Role::Viewer, "viewer-pass").unwrap();
        assert!(server.login("watch", "viewer-pass", REMOTE).is_ok());
        // And the server's next save keeps it.
        server.add("third", Role::Viewer, "third-pass").unwrap();
        assert_eq!(Accounts::load(path).list().len(), 3);
    }

    #[test]
    fn a_damaged_file_lets_nobody_in() {
        let path = temp();
        std::fs::write(&path, "{ not json").unwrap();
        let a = Accounts::load(path.clone());
        assert!(!a.is_open());
        assert!(a.problem().is_some());
        assert_eq!(a.who(LOCAL, None), None);
        assert!(a.add("nick", Role::Admin, "the-password").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json", "it's left as it is");
    }
}
