//! Termination signals as a flag a loop can poll.
//!
//! `ahi exec` must write its result file when the operator presses Ctrl+C,
//! when a coordinator sends SIGTERM, and when the ssh session that started
//! it drops (SIGHUP). `std` offers no way to be told about a signal, so this
//! in-repo crate holds the one `extern "C"` declaration and the one `unsafe`
//! call that `ahi` itself, which forbids unsafe code, cannot. It has no
//! dependencies.
//!
//! The handler does one async-signal-safe thing: it stores the signal
//! number in an atomic and restores the default disposition, so the first
//! signal is reported to the program and a second one ends the process the
//! way it always did. Nothing is allocated, locked, or printed in the
//! handler. The program polls [`received`] from its own loop.
//!
//! On other platforms the crate compiles to stubs: [`watch`] reports that
//! nothing is watched and [`received`] is always `None`.

use std::sync::atomic::{AtomicI32, Ordering};

/// A termination signal this crate reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// SIGINT: Ctrl+C at a terminal.
    Interrupt,
    /// SIGTERM: a polite request to stop, as from a coordinator.
    Terminate,
    /// SIGHUP: the controlling terminal or ssh session went away.
    Hangup,
}

impl Signal {
    /// The conventional name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Hangup => "SIGHUP",
        }
    }
}

/// Zero until a signal arrives, then its number. Written by the handler,
/// read by the program; an atomic is the one primitive both may touch.
static RECEIVED: AtomicI32 = AtomicI32::new(0);

// The numbers are the same on Linux, macOS, and the BSDs; they are part of
// each platform's stable ABI.
#[cfg(unix)]
const SIGHUP: i32 = 1;
#[cfg(unix)]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;
/// `SIG_DFL`: the default disposition, passed as a handler.
#[cfg(unix)]
const SIG_DFL: usize = 0;
/// `SIG_ERR`: `signal` could not install the handler.
#[cfg(unix)]
const SIG_ERR: usize = usize::MAX;

#[cfg(unix)]
extern "C" {
    /// POSIX `signal(2)`. The handler is passed and returned as a
    /// pointer-sized integer so `SIG_DFL` and `SIG_ERR` can be expressed
    /// without a second declaration; a function pointer fits the same word
    /// on every supported target.
    fn signal(signum: i32, handler: usize) -> usize;
}

/// The handler. Async-signal-safe: one atomic store and one `signal` call,
/// which POSIX lists among the functions a handler may call.
#[cfg(unix)]
extern "C" fn record(signum: i32) {
    RECEIVED.store(signum, Ordering::SeqCst);
    // SAFETY: `signal` is async-signal-safe; the arguments are a signal
    // number the kernel just delivered and the constant default disposition.
    unsafe {
        signal(signum, SIG_DFL);
    }
}

/// Install the handler for SIGINT, SIGTERM, and SIGHUP. Returns whether all
/// three were installed; a `false` means the process keeps the default
/// dispositions it had and the caller should say so.
#[cfg(unix)]
pub fn watch() -> bool {
    let handler = record as extern "C" fn(i32) as usize;
    [SIGINT, SIGTERM, SIGHUP].into_iter().all(|signum| {
        // SAFETY: `record` is an `extern "C" fn(i32)` that lives for the whole
        // program, does only async-signal-safe work, and is the documented
        // handler shape for `signal`. The call replaces a process-wide
        // disposition, which is the caller's stated intent.
        unsafe { signal(signum, handler) != SIG_ERR }
    })
}

/// Not Unix: nothing is watched.
#[cfg(not(unix))]
pub fn watch() -> bool {
    false
}

/// The first signal that arrived since the handler was installed, if any.
/// Stays set once set: the program decides what to do and exits.
pub fn received() -> Option<Signal> {
    #[cfg(unix)]
    {
        match RECEIVED.load(Ordering::SeqCst) {
            SIGINT => Some(Signal::Interrupt),
            SIGTERM => Some(Signal::Terminate),
            SIGHUP => Some(Signal::Hangup),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// One test only: the handler restores the default disposition after the
    /// first signal, so a second SIGTERM to this process would end it.
    #[test]
    fn a_watched_signal_is_reported_once_and_the_default_is_restored() {
        assert!(watch());
        assert_eq!(received(), None);
        let status = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while received().is_none() {
            assert!(std::time::Instant::now() < deadline, "signal not delivered");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(received(), Some(Signal::Terminate));
        assert_eq!(Signal::Terminate.name(), "SIGTERM");
        // The disposition is back to default: installing again succeeds and
        // returns to our handler without the process having died meanwhile.
        assert!(watch());
    }
}
