//! Two or more SDRplay radios in one process. The SDRplay API won't select a
//! second radio in a `sdrplay_api_Open()` session that already has one, and
//! SoapySDRPlay3 opens a single session per process, so a second RSP fails
//! with "SelectDevice() failed". As SDRTrunk does, open another session just
//! before a radio is opened while another is in use and the API's current
//! session (the one opened or released last) has a radio on it. A session that
//! stays idle (a failed open, a radio released) is reused, so retries and
//! restarts don't pile them up. Sessions are never closed: closing one makes
//! the others unusable. The open and the select must not interleave with
//! another radio's, hence the lock held until `made()`.
//!
//! The API library is the one SoapySDRPlay3 already loaded, never another
//! copy: sessions opened in a different copy wouldn't help.

use std::ffi::c_int;
use std::sync::{Mutex, MutexGuard, OnceLock};

use libloading::Library;

/// Held while an SDRplay radio opens. SoapySDRPlay3's device search shares
/// unsynchronized state with its open, so the device search takes it too.
pub static OPENING: Mutex<()> = Mutex::new(());
static IN_USE: Mutex<(usize, bool)> = Mutex::new((0, true)); // (radios in use, the API's current session has no radio on it)

/// One source's claim, from before its radio opens until it closes.
pub struct Slot {
    live: bool,
    /// The radio opened (so its release leaves the current session idle).
    opened: bool,
    opening: Option<MutexGuard<'static, ()>>,
}

/// Call before `make`, after the Soapy modules have loaded. Only sources whose
/// arguments name `driver=sdrplay` take part.
pub fn before_open(args: &str) -> Slot {
    let none = Slot { live: false, opened: false, opening: None };
    if !names_sdrplay(args) {
        return none;
    }
    let Some(open) = session_open() else { return none };
    let opening = OPENING.lock().unwrap_or_else(|e| e.into_inner());
    let mut st = IN_USE.lock().unwrap();
    if st.0 > 0 && !st.1 {
        // SAFETY: sdrplay_api_Open() takes no arguments.
        if unsafe { open() } == 0 {
            st.1 = true;
        }
    }
    st.0 += 1;
    Slot { live: true, opened: false, opening: Some(opening) }
}

impl Slot {
    /// The radio is open (or failed to): let the next one open.
    pub fn made(&mut self, opened: bool) {
        if self.live && opened {
            self.opened = true;
            IN_USE.lock().unwrap().1 = false;
        }
        self.opening = None;
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if self.live {
            let mut st = IN_USE.lock().unwrap();
            st.0 -= 1;
            if self.opened {
                st.1 = true; // the radio is released first (see `stream_once`): its session is the current one, idle
            }
        }
    }
}

fn names_sdrplay(args: &str) -> bool {
    args.split(',').filter_map(|kv| kv.split_once('=')).any(|(k, v)| k.trim().eq_ignore_ascii_case("driver") && v.trim().eq_ignore_ascii_case("sdrplay"))
}

/// `sdrplay_api_Open` from the API library already in the process, if any.
fn session_open() -> Option<unsafe extern "C" fn() -> c_int> {
    static OPEN: OnceLock<Option<(Library, unsafe extern "C" fn() -> c_int)>> = OnceLock::new();
    OPEN.get_or_init(|| {
        let lib = loaded()?;
        // SAFETY: the API's documented signature.
        let open = unsafe { *lib.get::<unsafe extern "C" fn() -> c_int>(b"sdrplay_api_Open\0").ok()? };
        Some((lib, open))
    })
    .as_ref()
    .map(|(_, open)| *open)
}

/// A handle to the API library only if it is already loaded (by SoapySDRPlay3).
fn loaded() -> Option<Library> {
    #[cfg(unix)]
    {
        use libloading::os::unix::{Library as Unix, RTLD_NOW};
        // SAFETY: RTLD_NOLOAD loads nothing, so no initialisers run.
        ["libsdrplay_api.so.3", "/usr/local/lib/libsdrplay_api.so.3", "libsdrplay_api.dylib"]
            .iter()
            .find_map(|n| unsafe { Unix::open(Some(n), RTLD_NOW | libc::RTLD_NOLOAD) }.ok())
            .map(Library::from)
    }
    #[cfg(windows)]
    {
        libloading::os::windows::Library::open_already_loaded("sdrplay_api.dll").ok().map(Library::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver() {
        assert!(names_sdrplay("driver=sdrplay,serial=240522FF60"));
        assert!(names_sdrplay("serial=1, Driver= SDRplay"));
        assert!(!names_sdrplay("driver=hackrf"));
        assert!(!names_sdrplay("serial=240522FF60"));
        assert!(!names_sdrplay(""));
    }
}
