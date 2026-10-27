//! Waking the main loop from other threads, and queueing the library's own
//! events.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sdl3_sys::events::{SDL_Event, SDL_PushEvent, SDL_UserEvent};
use sdl3_sys::video::SDL_WindowID;

use crate::{Result, support};

/// Wakes `App::wait` from any thread. Wakes that arrive while one is still
/// queued fold into it.
#[derive(Debug, Clone)]
pub struct Waker {
    pub(crate) wake_type: u32,
    // Set while a wake is queued and not yet taken. It outlives the `App`,
    // since wakers may: a waker used after `finalize` finds SDL gone and
    // answers an error.
    pub(crate) pending: Arc<AtomicBool>,
}

impl Waker {
    /// Queues one `Event::Wake` unless one is already queued.
    pub fn wake(&self) -> Result<()> {
        if self
            .pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let pushed = push_user_event(self.wake_type, 0, 0, 0, 0);
        if pushed.is_err() {
            self.pending.store(false, Ordering::Release);
        }
        pushed
    }
}

/// Queues one of the library's own events on the SDL queue.
pub(crate) fn push_user_event(
    kind: u32,
    window: u32,
    code: i32,
    data1: u64,
    data2: u64,
) -> Result<()> {
    // SDL copies the complete event union, including bytes outside the
    // active member, so the whole union starts zeroed.
    // SAFETY: an all-zero SDL_Event is a valid value of the union.
    let mut event: SDL_Event = unsafe { std::mem::zeroed() };
    event.user = SDL_UserEvent {
        r#type: kind,
        reserved: 0,
        timestamp: 0,
        windowID: SDL_WindowID(window),
        code,
        data1: data1 as usize as *mut std::ffi::c_void,
        data2: data2 as usize as *mut std::ffi::c_void,
    };
    // SAFETY: event is a fully initialized union SDL copies.
    support::check(unsafe { SDL_PushEvent(&mut event) }, "queueing an event")
}
