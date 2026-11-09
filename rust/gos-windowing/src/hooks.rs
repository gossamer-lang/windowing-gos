//! Callbacks SDL runs into the program: a window's hit test, and the redraw
//! a live resize asks for. Each boxed function lives in a property SDL owns,
//! with a cleanup SDL runs when the property is replaced or cleared, its
//! window closes, or the session ends. A redraw scoped to one pump lives in
//! a thread-local slot for the length of that pump instead.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::NonNull;

use sdl3_sys::events::{
    SDL_AddEventWatch, SDL_EVENT_WINDOW_EXPOSED, SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED,
    SDL_EVENT_WINDOW_RESIZED, SDL_Event, SDL_RemoveEventWatch, SDL_WindowEvent,
};
use sdl3_sys::properties::{
    SDL_GetGlobalProperties, SDL_GetPointerProperty, SDL_SetPointerProperty,
    SDL_SetPointerPropertyWithCleanup,
};
use sdl3_sys::rect::SDL_Point;
use sdl3_sys::video::{
    SDL_GetWindowProperties, SDL_HitTestResult, SDL_SetWindowHitTest, SDL_Window,
};

use crate::window::{self, HitArea, Window, WindowId};
use crate::{Result, support};

const HIT_TEST_PROPERTY: &std::ffi::CStr = c"windowing.hit_test";
const LIVE_RESIZE_PROPERTY: &std::ffi::CStr = c"windowing.live_resize";

type HitTest = Box<dyn Fn(f64, f64) -> HitArea + Send>;

unsafe extern "C" fn hit_test_entry(
    _window: *mut SDL_Window,
    point: *const SDL_Point,
    data: *mut c_void,
) -> SDL_HitTestResult {
    if data.is_null() || point.is_null() {
        return SDL_HitTestResult(0);
    }
    // SAFETY: data is the HitTest the property keeps alive; point is SDL's.
    let (test, at) = unsafe { (&*data.cast::<HitTest>(), &*point) };
    SDL_HitTestResult(window::hit_area_code(test(
        f64::from(at.x),
        f64::from(at.y),
    )))
}

unsafe extern "C" fn release_hit_test(_userdata: *mut c_void, value: *mut c_void) {
    if !value.is_null() {
        // SAFETY: value is the Box `set_hit_test` leaked; SDL releases it once.
        drop(unsafe { Box::from_raw(value.cast::<HitTest>()) });
    }
}

pub(crate) fn set_hit_test(handle: *mut SDL_Window, area: Option<HitTest>) -> Result<()> {
    // SAFETY: handle is a live window.
    let props = unsafe { SDL_GetWindowProperties(handle) };
    let installed = match area {
        Some(test) => {
            let context = Box::into_raw(Box::new(test)).cast::<c_void>();
            // SAFETY: the context stays valid until the property's cleanup.
            if !unsafe { SDL_SetWindowHitTest(handle, Some(hit_test_entry), context) } {
                // SAFETY: SDL refused it, so this is the only owner.
                unsafe { release_hit_test(std::ptr::null_mut(), context) };
                return Err(support::failure("installing the hit test"));
            }
            context
        }
        None => {
            support::check(
                // SAFETY: handle is live.
                unsafe { SDL_SetWindowHitTest(handle, Some(hit_test_entry), std::ptr::null_mut()) },
                "removing the hit test",
            )?;
            std::ptr::null_mut()
        }
    };
    // Replacing the property releases the test it held, which SDL no longer
    // calls; a failed set releases the new one.
    support::check(
        // SAFETY: props is live; the cleanup matches the stored value.
        unsafe {
            SDL_SetPointerPropertyWithCleanup(
                props,
                HIT_TEST_PROPERTY.as_ptr(),
                installed,
                Some(release_hit_test),
                std::ptr::null_mut(),
            )
        },
        "keeping the hit test",
    )
}

/// The redraw a live resize runs, with the event type redraws carry.
pub(crate) struct LiveResize {
    pub(crate) redraw: Box<dyn Fn(Window) + Send>,
    pub(crate) redraw_type: u32,
}

type ScopedRedraw = dyn FnMut(Window);

thread_local! {
    // The redraw `redrawing` installs, with the event type redraws carry.
    static SCOPED: Cell<Option<(NonNull<ScopedRedraw>, u32)>> = const { Cell::new(None) };
    // A redraw is running; a pump inside it must not start another.
    static REDRAWING: Cell<bool> = const { Cell::new(false) };
    // The library's own wait or poll is pumping events.
    static PUMPING: Cell<bool> = const { Cell::new(false) };
}

// Whether `e` asks for a redraw that the platform's loop gives no other
// chance to paint: the expose a live resize sends while that loop blocks the
// program, or a size change where the frame is a macOS layer. Cocoa delivers
// a size change inside the resize's own Core Animation transaction, so a frame
// painted at once shows together with the new size. SDL's window surface is
// replaced only after its watches run, so a size change does not ask there.
fn asks_live_redraw(e: &SDL_WindowEvent) -> bool {
    if e.r#type == SDL_EVENT_WINDOW_EXPOSED {
        return e.data1 != 0;
    }
    (e.r#type == SDL_EVENT_WINDOW_RESIZED || e.r#type == SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED)
        && paints_on_resize()
}

#[cfg(target_os = "macos")]
fn paints_on_resize() -> bool {
    crate::macos::layer::applies()
}

#[cfg(not(target_os = "macos"))]
fn paints_on_resize() -> bool {
    false
}

// Runs `redraw` unless a redraw is already running on this thread.
fn redraw_once(redraw: impl FnOnce()) {
    if REDRAWING.replace(true) {
        return;
    }
    redraw();
    REDRAWING.set(false);
}

/// Runs `pump` with the library's wait or poll marked as pumping events.
pub(crate) fn pumping<R>(pump: impl FnOnce() -> R) -> R {
    let outer = PUMPING.replace(true);
    let answer = pump();
    PUMPING.set(outer);
    answer
}

/// Runs `pump` with `redraw` answering the redraws a live resize asks for
/// meanwhile.
pub(crate) fn redrawing<R>(
    redraw: &mut dyn FnMut(Window),
    redraw_type: u32,
    pump: impl FnOnce() -> R,
) -> R {
    struct Restore {
        outer: Option<(NonNull<ScopedRedraw>, u32)>,
        watching: bool,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            SCOPED.set(self.outer);
            if self.watching {
                // SAFETY: the watch takes no context, and is the one added below.
                unsafe { SDL_RemoveEventWatch(Some(scoped_redraw_entry), std::ptr::null_mut()) };
            }
        }
    }
    let borrowed: NonNull<dyn FnMut(Window) + '_> = NonNull::from(redraw);
    // SAFETY: only the lifetime changes. The slot holds the pointer until
    // `Restore` drops at the end of this call, while `redraw` stays borrowed.
    let erased: NonNull<ScopedRedraw> = unsafe { std::mem::transmute(borrowed) };
    let outer = SCOPED.replace(Some((erased, redraw_type)));
    // SDL runs every watch on every event, so this one is installed only for
    // the scopes that want it. Without it, the pump runs with no live redraw.
    // SAFETY: the watch takes no context.
    let watching = outer.is_none()
        && unsafe { SDL_AddEventWatch(Some(scoped_redraw_entry), std::ptr::null_mut()) };
    let _restore = Restore { outer, watching };
    pump()
}

// SDL runs event watches for every event, on whatever thread queued it; the
// slot is set only on the thread pumping inside `redrawing`.
unsafe extern "C" fn scoped_redraw_entry(_data: *mut c_void, event: *mut SDL_Event) -> bool {
    if event.is_null() {
        return true;
    }
    // SAFETY: event is a complete SDL event; the window member is read only
    // for a window event type.
    let e = unsafe { &(*event).window };
    if !asks_live_redraw(e) {
        return true;
    }
    let Some((redraw, redraw_type)) = SCOPED.get() else {
        return true;
    };
    let Some(win) = window::from_id(WindowId(e.windowID.0), redraw_type) else {
        return true;
    };
    redraw_once(|| {
        // SAFETY: the slot holds an exclusive borrow live for the pump this
        // event came from, and `redraw_once` keeps this the only call into it.
        unsafe { (*redraw.as_ptr())(win) }
    });
    true
}

// SDL runs event watches for every event, on whatever thread queued it. The
// expose a live resize sends comes on the main thread, inside the modal loop
// the platform runs while the user drags; a size change on a macOS layer asks
// too while the library's own wait or poll pumps, never from a resize the
// program makes.
unsafe extern "C" fn live_resize_entry(data: *mut c_void, event: *mut SDL_Event) -> bool {
    if data.is_null() || event.is_null() {
        return true;
    }
    // SAFETY: event is a complete SDL event; the window member is read only
    // for a window event type.
    let e = unsafe { &(*event).window };
    if !asks_live_redraw(e) || (e.r#type != SDL_EVENT_WINDOW_EXPOSED && !PUMPING.get()) {
        return true;
    }
    // A redraw scoped to this pump paints the frame instead.
    if SCOPED.get().is_some() {
        return true;
    }
    // SAFETY: data is the LiveResize the global property keeps alive.
    let live = unsafe { &*data.cast::<LiveResize>() };
    if let Some(win) = window::from_id(WindowId(e.windowID.0), live.redraw_type) {
        redraw_once(|| (live.redraw)(win));
    }
    true
}

unsafe extern "C" fn release_live_resize(_userdata: *mut c_void, value: *mut c_void) {
    if !value.is_null() {
        // SAFETY: value is the Box `add_live_resize` leaked; SDL releases it once.
        drop(unsafe { Box::from_raw(value.cast::<LiveResize>()) });
    }
}

pub(crate) fn add_live_resize(hook: LiveResize) -> Result<()> {
    let context = Box::into_raw(Box::new(hook)).cast::<c_void>();
    // SAFETY: the context stays valid until the property's cleanup.
    if !unsafe { SDL_AddEventWatch(Some(live_resize_entry), context) } {
        // SAFETY: SDL refused it, so this is the only owner.
        unsafe { release_live_resize(std::ptr::null_mut(), context) };
        return Err(support::failure("installing the live-resize hook"));
    }
    // SAFETY: the global property set lives while SDL runs.
    let kept = unsafe {
        SDL_SetPointerPropertyWithCleanup(
            SDL_GetGlobalProperties(),
            LIVE_RESIZE_PROPERTY.as_ptr(),
            context,
            Some(release_live_resize),
            std::ptr::null_mut(),
        )
    };
    if !kept {
        // SDL released the context already; the watch must not outlive it.
        // SAFETY: removes the watch added above.
        unsafe { SDL_RemoveEventWatch(Some(live_resize_entry), context) };
        return Err(support::failure("keeping the live-resize hook"));
    }
    Ok(())
}

pub(crate) fn remove_live_resize() -> Result<()> {
    // SAFETY: the global property set lives while SDL runs.
    let props = unsafe { SDL_GetGlobalProperties() };
    // SAFETY: SDL answers null when unset.
    let context = unsafe {
        SDL_GetPointerProperty(props, LIVE_RESIZE_PROPERTY.as_ptr(), std::ptr::null_mut())
    };
    if context.is_null() {
        return Ok(());
    }
    // SAFETY: removes the watch registered with this context.
    unsafe { SDL_RemoveEventWatch(Some(live_resize_entry), context) };
    // Clearing the property releases the hook the watch held.
    support::check(
        // SAFETY: props is live.
        unsafe {
            SDL_SetPointerProperty(props, LIVE_RESIZE_PROPERTY.as_ptr(), std::ptr::null_mut())
        },
        "removing the live-resize hook",
    )
}

/// Keeps the X11 library's process-wide caches owned across sessions.
///
/// Xlib owns locale and quark caches with no public teardown; SDL closes its
/// display connections at shutdown, then unloads Xlib. Pinning the library
/// SDL already loaded keeps those caches owned and reusable. This holds no
/// SDL session or display connection open.
#[cfg(free_unix)]
pub(crate) fn preserve_x11_owners() -> bool {
    // NOLOAD promotes only the library SDL already loaded. NODELETE keeps
    // its static storage alive; close balances the temporary loader ref.
    // SAFETY: dlopen with NOLOAD loads nothing new; the handle is closed at once.
    unsafe {
        let library = libc::dlopen(
            c"libX11.so.6".as_ptr(),
            libc::RTLD_LAZY | libc::RTLD_NOLOAD | libc::RTLD_NODELETE,
        );
        !library.is_null() && libc::dlclose(library) == 0
    }
}

/// SDL keeps one X11 event hook. Each part of the library that reads raw X
/// events subscribes here, and the one hook offers every event to each
/// subscriber until one consumes it.
#[cfg(free_unix)]
pub(crate) mod x11_events {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU8, Ordering};

    use sdl3_sys::system::{SDL_SetX11EventHook, XEvent};

    pub(crate) const DRAG: u8 = 1;
    pub(crate) const HOTKEYS: u8 = 2;

    static SUBSCRIBERS: AtomicU8 = AtomicU8::new(0);

    // SDL handles the event only when this answers true.
    unsafe extern "C" fn on_event(_userdata: *mut c_void, event: *mut XEvent) -> bool {
        let subscribed = SUBSCRIBERS.load(Ordering::Acquire);
        (subscribed & DRAG == 0 || crate::drag::on_x11_event(event))
            && (subscribed & HOTKEYS == 0 || crate::hotkey::on_x11_event(event))
    }

    /// Offers X events to `subscriber` from now on. Main thread only.
    pub(crate) fn subscribe(subscriber: u8) {
        if SUBSCRIBERS.fetch_or(subscriber, Ordering::AcqRel) == 0 {
            // SAFETY: the hook is a plain function that lives for the program.
            unsafe { SDL_SetX11EventHook(Some(on_event), std::ptr::null_mut()) };
        }
    }

    /// Stops offering X events to `subscriber`. Main thread only.
    pub(crate) fn unsubscribe(subscriber: u8) {
        if SUBSCRIBERS.fetch_and(!subscriber, Ordering::AcqRel) == subscriber {
            // SAFETY: clears the hook SDL calls.
            unsafe { SDL_SetX11EventHook(None, std::ptr::null_mut()) };
        }
    }
}

/// SDL keeps one Windows message hook, shared the same way as the X11 one.
#[cfg(target_os = "windows")]
pub(crate) mod windows_messages {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU8, Ordering};

    use sdl3_sys::system::{MSG, SDL_SetWindowsMessageHook};

    pub(crate) const PREFERENCES: u8 = 1;
    pub(crate) const HOTKEYS: u8 = 2;

    static SUBSCRIBERS: AtomicU8 = AtomicU8::new(0);

    unsafe extern "C" fn on_message(_userdata: *mut c_void, message: *mut MSG) -> bool {
        // SAFETY: SDL hands a complete message.
        let m = unsafe { &*message.cast::<windows_sys::Win32::UI::WindowsAndMessaging::MSG>() };
        let subscribed = SUBSCRIBERS.load(Ordering::Acquire);
        if subscribed & PREFERENCES != 0 {
            crate::system::prefs::on_message(m);
        }
        if subscribed & HOTKEYS != 0 {
            crate::hotkey::on_windows_message(m);
        }
        true
    }

    /// Offers thread and window messages to `subscriber` from now on.
    pub(crate) fn subscribe(subscriber: u8) {
        if SUBSCRIBERS.fetch_or(subscriber, Ordering::AcqRel) == 0 {
            // SAFETY: the hook is a plain function that lives for the program.
            unsafe { SDL_SetWindowsMessageHook(Some(on_message), std::ptr::null_mut()) };
        }
    }

    /// Stops offering messages to `subscriber`.
    pub(crate) fn unsubscribe(subscriber: u8) {
        if SUBSCRIBERS.fetch_and(!subscriber, Ordering::AcqRel) == subscriber {
            // SAFETY: clears the hook SDL calls.
            unsafe { SDL_SetWindowsMessageHook(None, std::ptr::null_mut()) };
        }
    }
}
