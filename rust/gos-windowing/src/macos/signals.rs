//! Wakes the main loop when the process is asked to quit by a signal. SDL's
//! SIGTERM and SIGINT handler only marks a quit for the next time events are
//! pumped, and an idle wait in the AppKit run loop is not interrupted by a
//! signal, so without a wake the quit waits for the next unrelated event.

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::waker::Waker;

unsafe extern "C" {
    // DISPATCH_SOURCE_TYPE_SIGNAL and dispatch_get_main_queue() are the
    // addresses of these libdispatch globals.
    static _dispatch_source_type_signal: c_void;
    static _dispatch_main_q: c_void;
    fn dispatch_source_create(
        kind: *const c_void,
        handle: usize,
        mask: usize,
        queue: *const c_void,
    ) -> *mut c_void;
    fn dispatch_source_set_event_handler_f(
        source: *mut c_void,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_resume(object: *mut c_void);
}

static WAKER: OnceLock<Waker> = OnceLock::new();

// Runs on the main queue, which the AppKit run loop drains while SDL waits.
// Queueing an event makes SDL post its wakeup event, so the wait returns and
// the next pump delivers the quit SDL's handler marked.
extern "C" fn quit_signal(_context: *mut c_void) {
    // A wake fails only once SDL has shut down, when there is no wait to end.
    if let Some(waker) = WAKER.get() {
        let _ = waker.wake();
    }
}

/// Wakes `waker`'s loop on every SIGTERM and SIGINT, once per process. A
/// kqueue signal source records a signal alongside the handler SDL installs,
/// so SDL still marks the quit itself.
pub(crate) fn wake_on_quit_signals(waker: Waker) {
    if WAKER.set(waker).is_err() {
        return;
    }
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the source type and the main queue are libdispatch's own
        // globals, and a source created here lives for the process.
        unsafe {
            let source = dispatch_source_create(
                &raw const _dispatch_source_type_signal,
                signal as usize,
                0,
                &raw const _dispatch_main_q,
            );
            if source.is_null() {
                continue;
            }
            dispatch_source_set_event_handler_f(source, quit_signal);
            dispatch_resume(source);
        }
    }
}
