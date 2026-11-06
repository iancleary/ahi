//! Native Windows clipboard through the Win32 API, with no process or file.
//!
//! This in-repo crate exists so `ahi` itself can keep `unsafe_code = "forbid"`:
//! it holds the only `unsafe` in the workspace and no dependencies. Every call
//! is documented Win32 behavior: the clipboard is opened without an owning
//! window, emptied, given one moveable `CF_UNICODETEXT` block the system now
//! owns, and closed. Nothing dereferences memory it did not allocate.
//! On other platforms the crate compiles to a stub that always reports failure.
#[cfg(windows)]
use std::ffi::c_void;

/// Not Windows: the caller falls through to its next adapter.
#[cfg(not(windows))]
pub fn copy(_text: &str) -> bool {
    false
}

#[cfg(windows)]
type Handle = *mut c_void;
#[cfg(windows)]
type Bool = i32;

#[cfg(windows)]
const CF_UNICODETEXT: u32 = 13;
#[cfg(windows)]
const GMEM_MOVEABLE: u32 = 0x0002;
/// Another process may hold the clipboard briefly after its own write.
#[cfg(windows)]
const OPEN_ATTEMPTS: u32 = 10;

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(owner: Handle) -> Bool;
    fn EmptyClipboard() -> Bool;
    fn SetClipboardData(format: u32, data: Handle) -> Handle;
    fn CloseClipboard() -> Bool;
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    fn GlobalLock(memory: Handle) -> *mut c_void;
    fn GlobalUnlock(memory: Handle) -> Bool;
    fn GlobalFree(memory: Handle) -> Handle;
}

/// Writes `text` as UTF-16 to the system clipboard. Returns false if the
/// clipboard stayed busy, allocation failed, or Windows refused the data.
#[cfg(windows)]
pub fn copy(text: &str) -> bool {
    let utf16 = encode(text);
    let bytes = utf16.len().saturating_mul(2);
    if !open_clipboard() {
        return false;
    }
    // SAFETY: the clipboard is open on this thread (checked above). `memory`
    // is a fresh moveable block of exactly `bytes`, locked for the copy, then
    // unlocked. On success the system owns the block, so it is not freed; on
    // failure it is freed exactly once. The clipboard is closed on every path.
    unsafe {
        let copied = (|| {
            if EmptyClipboard() == 0 {
                return false;
            }
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if memory.is_null() {
                return false;
            }
            let locked = GlobalLock(memory);
            if locked.is_null() {
                GlobalFree(memory);
                return false;
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), locked.cast::<u16>(), utf16.len());
            GlobalUnlock(memory);
            if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
                GlobalFree(memory);
                return false;
            }
            true
        })();
        CloseClipboard();
        copied
    }
}

#[cfg(windows)]
fn open_clipboard() -> bool {
    for attempt in 0..OPEN_ATTEMPTS {
        // SAFETY: a null owner is documented as "the current task"; no memory is involved.
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            return true;
        }
        if attempt + 1 < OPEN_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    false
}

/// `CF_UNICODETEXT` is NUL-terminated UTF-16 with CRLF line endings. Bare LF
/// is normalized so Windows editors see line breaks; existing CRLF is kept.
#[cfg(any(windows, test))]
fn encode(text: &str) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut previous = '\0';
    for ch in text.chars() {
        if ch == '\n' && previous != '\r' {
            out.push(u16::from(b'\r'));
        }
        let mut units = [0; 2];
        out.extend_from_slice(ch.encode_utf16(&mut units));
        previous = ch;
    }
    out.push(0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_or_native_copy_never_panics() {
        let _ = copy("");
    }

    #[test]
    fn encode_normalizes_line_endings_and_terminates() {
        let units = encode("a\nb\r\nc 界🙂\n");
        let expected: Vec<u16> = "a\r\nb\r\nc 界🙂\r\n\0".encode_utf16().collect();
        assert_eq!(units, expected);
        assert_eq!(encode(""), [0]);
    }

    #[cfg(windows)]
    #[test]
    fn copy_round_trips_through_the_system_clipboard() {
        // Exercised on Windows CI only; the clipboard is global process state,
        // so this test may observe another writer and is kept to a single call.
        assert!(copy("ahi clipboard test 界🙂\n"));
    }
}
