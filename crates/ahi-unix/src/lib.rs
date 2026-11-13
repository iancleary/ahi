//! The two POSIX calls `ahi` needs and `std` does not expose: a signal
//! handler that records a termination signal, and `kill` aimed at a whole
//! process group.
//!
//! This in-repo crate exists so `ahi` itself can keep `unsafe_code = "forbid"`:
//! it declares each libc function it calls and has no dependencies, the same
//! shape as `ahi-win32`. On other platforms it compiles to stubs: nothing is
//! watched, nothing is received, and the group kill reports that it did
//! nothing, so the caller falls back to killing the one child it owns.
//!
//! The signal handler does one async-signal-safe thing: it stores the signal
//! number in an atomic, first arrival wins, and restores the default
//! disposition for that signal, so a second copy of it ends the process the
//! way it always did. Nothing is allocated, locked, or printed in the
//! handler. The program polls [`received`] from its own loop.

use std::sync::atomic::{AtomicI32, Ordering};

/// A termination signal this crate can watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// SIGINT: Ctrl+C at a terminal.
    Interrupt,
    /// SIGTERM: a polite request to stop, as from a supervisor.
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

    // The numbers are the same on Linux, macOS, and the BSDs; they are part
    // of each platform's stable ABI.
    fn number(self) -> i32 {
        match self {
            Self::Hangup => 1,
            Self::Interrupt => 2,
            Self::Terminate => 15,
        }
    }

    fn from_number(number: i32) -> Option<Self> {
        [Self::Hangup, Self::Interrupt, Self::Terminate]
            .into_iter()
            .find(|signal| signal.number() == number)
    }
}

/// Zero until a signal arrives, then its number. Written by the handler,
/// read by the program; an atomic is the one primitive both may touch.
static RECEIVED: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
const SIGKILL: i32 = 9;
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
    /// POSIX `kill(2)`. A negative `pid` names a process group.
    fn kill(pid: i32, sig: i32) -> i32;
}

/// The handler. Async-signal-safe: one atomic exchange and one `signal`
/// call, which POSIX lists among the functions a handler may call.
#[cfg(unix)]
extern "C" fn record(signum: i32) {
    let _ = RECEIVED.compare_exchange(0, signum, Ordering::SeqCst, Ordering::SeqCst);
    // SAFETY: `signal` is async-signal-safe; the arguments are a signal
    // number the kernel just delivered and the constant default disposition.
    unsafe {
        signal(signum, SIG_DFL);
    }
}

/// Install the handler for `signals`. Returns whether every one was
/// installed; a `false` means some dispositions are unchanged and the caller
/// should say so. Signals not listed keep their default behaviour, so a
/// server can watch SIGTERM and SIGINT while a dropped terminal still ends
/// it as usual.
#[cfg(unix)]
pub fn watch(signals: &[Signal]) -> bool {
    let handler = record as extern "C" fn(i32) as usize;
    signals.iter().all(|signal_| {
        // SAFETY: `record` is an `extern "C" fn(i32)` that lives for the whole
        // program, does only async-signal-safe work, and is the documented
        // handler shape for `signal`. The call replaces a process-wide
        // disposition, which is the caller's stated intent.
        unsafe { signal(signal_.number(), handler) != SIG_ERR }
    })
}

/// Not Unix: nothing is watched.
#[cfg(not(unix))]
pub fn watch(_signals: &[Signal]) -> bool {
    false
}

/// The first watched signal that arrived, if any. Stays set once set: the
/// program decides what to do and exits.
pub fn received() -> Option<Signal> {
    Signal::from_number(RECEIVED.load(Ordering::SeqCst))
}

/// Send SIGKILL to the process group whose leader is `pid`: a child started
/// with `CommandExt::process_group(0)` and everything it spawned. Returns
/// whether the kernel accepted the request; `false` when the group is
/// already gone or `pid` is not a group this process may signal.
#[cfg(unix)]
pub fn kill_process_group(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` takes two integers and touches no memory. A negative
    // pid addresses the group with that id; permission is the kernel's check.
    unsafe { kill(-pid, SIGKILL) == 0 }
}

/// Not Unix: no process groups; the caller kills the one child it owns.
#[cfg(not(unix))]
pub fn kill_process_group(_pid: u32) -> bool {
    false
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// One signal test only: the handler restores the default disposition
    /// after the first signal, so a second SIGTERM to this process would end
    /// it, and tests in one binary share the process.
    #[test]
    fn a_watched_signal_is_reported_once_and_the_default_is_restored() {
        assert!(watch(&[Signal::Terminate]));
        assert_eq!(received(), None);
        let status = Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(5);
        while received().is_none() {
            assert!(Instant::now() < deadline, "signal not delivered");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(received(), Some(Signal::Terminate));
        assert_eq!(Signal::Terminate.name(), "SIGTERM");
        // The disposition is back to default: installing again succeeds and
        // returns to our handler without the process having died meanwhile.
        assert!(watch(&[Signal::Terminate]));
    }

    #[test]
    fn killing_the_group_ends_the_child_and_what_it_spawned() {
        // The shell starts a grandchild that would outlive a kill aimed at
        // the shell alone; it holds the pipe open, which is how a leaked
        // grandchild is observed: the read does not end.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & sleep 30"])
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        assert!(kill_process_group(child.id()));
        let started = Instant::now();
        let mut sink = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut sink).unwrap();
        child.wait().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the grandchild kept the pipe open"
        );
        assert!(!kill_process_group(child.id()), "the group is gone");
        assert!(!kill_process_group(0));
    }
}
