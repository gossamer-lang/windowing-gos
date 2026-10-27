use std::ffi::{CStr, CString, c_char, c_void};

use sdl3_sys::init::SDL_IsMainThread;
use sdl3_sys::stdinc::SDL_free;

use crate::{Error, Result};

/// The error for a failed SDL call: what was attempted, then SDL's reason.
pub(crate) fn failure(op: &str) -> Error {
    // SAFETY: SDL_GetError always answers a valid NUL-terminated string.
    let reason = unsafe { CStr::from_ptr(sdl3_sys::error::SDL_GetError()) }.to_string_lossy();
    if reason.is_empty() {
        Error::new(op)
    } else {
        Error::new(format!("{op}: {reason}"))
    }
}

/// `text` as a C string; an error naming `what` when the text holds a NUL.
pub(crate) fn c_text(text: &str, what: &str) -> Result<CString> {
    CString::new(text).map_err(|_| Error::new(format!("{what} holds a NUL character")))
}

/// The text of a C string SDL keeps ownership of; empty for NULL.
pub(crate) fn read_text(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: a non-null SDL string is NUL-terminated and valid for the call.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// The text of a C string SDL hands over to the caller, freed once copied.
pub(crate) fn take_text(text: *mut c_char) -> String {
    let out = read_text(text);
    if !text.is_null() {
        // SAFETY: SDL allocated this string for the caller to free.
        unsafe { SDL_free(text.cast::<c_void>()) }
    }
    out
}

/// Panics unless the caller is on the thread that started video: SDL and the
/// platform window systems under it are bound to that thread.
pub(crate) fn require_main_thread(op: &str) {
    // SAFETY: SDL_IsMainThread only reads thread identity.
    if !unsafe { SDL_IsMainThread() } {
        panic!(
            "windowing: `{op}` must run on the main thread; wake it from a goroutine with a Waker"
        );
    }
}

/// `value` as a C `int`, or an error naming `what` when it does not fit.
pub(crate) fn c_int(value: i64, what: &str) -> Result<i32> {
    i32::try_from(value)
        .map_err(|_| Error::new(format!("{what} {value} is outside the range a C int holds")))
}

/// Succeeds when an SDL call reported success, else the error for `op`.
pub(crate) fn check(ok: bool, op: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(failure(op)) }
}
