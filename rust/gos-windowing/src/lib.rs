//! Windows and input for Gossamer's GUI stack, on SDL 3: windows, displays,
//! keyboard and text input, pointer, touch, and pen, the clipboard, drag and
//! drop, cursors, file dialogs and message boxes, and the surfaces a renderer
//! draws to. It draws nothing itself.
//!
//! Every call that touches a window runs on the main thread; other threads
//! reach the loop through a `Waker`.

pub mod access;
#[cfg(target_os = "macos")]
mod axmac;
#[cfg(target_os = "linux")]
pub mod bus;
pub mod capabilities;
pub mod clipboard;
pub mod compositor;
pub mod cursor;
pub mod dialog;
pub mod display;
pub mod drag;
mod error;
pub mod event;
mod frame;
mod hooks;
pub mod idle;
pub mod instance;
pub mod keys;
mod macos;
pub mod menu;
pub mod notify;
mod ole;
mod simulate;
mod support;
pub mod surface;
pub mod system;
pub mod tray;
#[cfg(target_os = "windows")]
mod uia;
mod waker;
mod win32;
pub mod window;
#[cfg(target_os = "linux")]
mod xlib;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sdl3_sys::events::{SDL_Event, SDL_PollEvent, SDL_RegisterEvents, SDL_WaitEventTimeout};
use sdl3_sys::hints::{
    SDL_HINT_FRAMEBUFFER_ACCELERATION, SDL_HINT_QUIT_ON_LAST_WINDOW_CLOSE,
    SDL_HINT_VIDEO_ALLOW_SCREENSAVER, SDL_HINT_VIDEO_DRIVER, SDL_SetHint,
};
use sdl3_sys::init::{SDL_INIT_VIDEO, SDL_Init, SDL_Quit, SDL_SetAppMetadata, SDL_WasInit};
use sdl3_sys::keyboard::{
    SDL_GetKeyFromScancode, SDL_GetKeyName, SDL_GetKeyboardState, SDL_GetModState,
};
use sdl3_sys::keycode::{SDL_KMOD_NONE, SDL_Keycode};
use sdl3_sys::mouse::{SDL_CaptureMouse, SDL_GetMouseState};
use sdl3_sys::scancode::SDL_Scancode;
use sdl3_sys::version::SDL_GetVersion;
use sdl3_sys::video::SDL_GetCurrentVideoDriver;

pub use capabilities::{Capabilities, Capability, Support, capabilities};
pub use cursor::{Cursor, SystemCursor};
pub use dialog::{DialogChoice, DialogId, FileDialog, Filter, MessageKind, Question};
pub use display::{Display, DisplayId, Mode, Orientation, Rect};
pub use drag::{DragId, DragItem, DragOperation};
pub use error::{Error, Result};
pub use event::{Buttons, Event, Mods, MouseButton, PenAxis, PenState, PointerSource};
pub use instance::{Claim, Request as InstanceRequest};
pub use keys::{Key, Scancode};
pub use menu::{Menu, MenuEntry, MenuSource, Shortcut, TrayId};
pub use notify::{Notification, Urgency};
pub use surface::{Frame, GlContext, GlOptions, GlProfile, MetalView, PixelFormat, RawHandle};
pub use system::{Locale, Theme};
pub use tray::{Tray, TrayIcon};
pub use waker::Waker;
pub use window::{
    Attention, Backdrop, Capitalization, Graphics, HitArea, TextInputCapabilities,
    TextInputOptions, TextPurpose,
};
pub use window::{Window, WindowId, WindowKind, WindowOptions};

/// The library's version.
pub const VERSION: &str = "0.1.0";

/// The running windowing session. There is one at a time; `finalize` ends
/// it and closes every window. Copies refer to the same session.
#[derive(Debug, Clone)]
pub struct App {
    pub(crate) event_base: u32,
    pending_wake: Arc<AtomicBool>,
}

/// How `init` starts the session.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// The application's reverse-DNS identity (Wayland app id, X11 class).
    pub app_id: String,
    pub version: String,
    /// A video driver by name (`"x11"`, `"wayland"`, `"offscreen"`); the
    /// platform's best when empty, and `SDL_VIDEO_DRIVER` overrides it.
    pub driver: String,
}

/// Starts windowing on the main thread. `name` is the application's name as
/// the desktop shows it.
pub fn init(name: &str, options: &InitOptions) -> Result<App> {
    // SAFETY: plain query.
    if unsafe { SDL_WasInit(SDL_INIT_VIDEO) }.0 != 0 {
        return Err(Error::new(
            "windowing is already running; finalize that App first",
        ));
    }
    let name_text = support::c_text(name, "the application name")?;
    let version_text = support::c_text(&options.version, "the application version")?;
    let id_text = support::c_text(&options.app_id, "the application id")?;
    // SAFETY: SDL copies the strings.
    unsafe { SDL_SetAppMetadata(name_text.as_ptr(), version_text.as_ptr(), id_text.as_ptr()) };
    // Windows close one by one at the program's say; quitting is the
    // program's decision too, on `Event::QuitRequested` or its own.
    set_hint(
        SDL_HINT_QUIT_ON_LAST_WINDOW_CLOSE,
        "SDL_QUIT_ON_LAST_WINDOW_CLOSE",
        "0",
    )?;
    // A native application leaves the screensaver alone unless it asks;
    // `system::inhibit_screensaver` asks.
    set_hint(
        SDL_HINT_VIDEO_ALLOW_SCREENSAVER,
        "SDL_VIDEO_ALLOW_SCREENSAVER",
        "1",
    )?;
    if !options.driver.is_empty() {
        set_hint(SDL_HINT_VIDEO_DRIVER, "SDL_VIDEO_DRIVER", &options.driver)?;
    }
    // SAFETY: called on the thread that becomes the UI thread.
    support::check(unsafe { SDL_Init(SDL_INIT_VIDEO) }, "starting video")?;
    if !preserve_driver_globals(&video_driver()) {
        // SAFETY: undoes the init above.
        unsafe { SDL_Quit() };
        return Err(Error::new(
            "preserving the X11 library's process-wide cache owners",
        ));
    }
    software_framebuffer_hint(&video_driver())?;
    drag::init();
    // SAFETY: plain registration.
    let base = unsafe { SDL_RegisterEvents(event::USER_EVENT_COUNT) };
    if base == 0 {
        // SAFETY: undoes the init above.
        unsafe { SDL_Quit() };
        return Err(Error::new(
            "registering the library's events: SDL has no event types left",
        ));
    }
    system::PREFERENCES_EVENT_TYPE.store(base + event::PREFERENCES_OFFSET, Ordering::Release);
    access::set_wake_event(base + event::WAKE_OFFSET);
    if let Err(e) = hooks::add_scoped_redraw() {
        // SAFETY: undoes the init above.
        unsafe { SDL_Quit() };
        return Err(e);
    }
    Ok(App {
        event_base: base,
        pending_wake: Arc::new(AtomicBool::new(false)),
    })
}

#[cfg(target_os = "linux")]
fn preserve_driver_globals(driver: &str) -> bool {
    driver != "x11" || hooks::preserve_x11_owners()
}

#[cfg(not(target_os = "linux"))]
fn preserve_driver_globals(_driver: &str) -> bool {
    true
}

// A window's framebuffer is drawn in software. The X11 and offscreen drivers
// present it through a framebuffer of their own (X11 shared memory, plain
// memory offscreen), which keeps SDL from backing it with an OpenGL renderer
// that loads the GL driver into the process; offscreen on macOS has no GL
// driver at all. Wayland has no such framebuffer and always presents through
// a renderer. SDL's unaccelerated framebuffer presents nothing on recent
// macOS, and the other platforms' accelerated paths load no GL driver.
fn software_framebuffer_hint(driver: &str) -> Result<()> {
    if driver == "offscreen" || (cfg!(target_os = "linux") && driver == "x11") {
        return set_hint(
            SDL_HINT_FRAMEBUFFER_ACCELERATION,
            "SDL_FRAMEBUFFER_ACCELERATION",
            "0",
        );
    }
    Ok(())
}

// A hint the user set in the environment takes priority over the program's,
// and SDL refuses the program's then; the user's choice standing is success.
fn set_hint(name: *const std::ffi::c_char, variable: &str, value: &str) -> Result<()> {
    let text = support::c_text(value, "a hint value")?;
    // SAFETY: name is SDL's NUL-terminated hint name; SDL copies the value.
    if unsafe { SDL_SetHint(name, text.as_ptr()) } || std::env::var_os(variable).is_some() {
        return Ok(());
    }
    Err(support::failure(&format!("setting {variable}")))
}

fn new_event() -> SDL_Event {
    // SAFETY: an all-zero SDL_Event is a valid value of the union.
    unsafe { std::mem::zeroed() }
}

impl App {
    /// Ends the session: closes every window and releases the platform.
    pub fn finalize(&self) {
        support::require_main_thread("App::finalize");
        dialog::shutdown();
        drag::shutdown();
        system::shutdown();
        win32::shutdown();
        #[cfg(target_os = "macos")]
        macos::layer::release_all();
        // Quitting removes the live-resize watch and destroys the property
        // holding its hook, which releases the hook.
        // SAFETY: ends the session on the main thread.
        unsafe { SDL_Quit() };
        simulate::release_all_injected();
    }

    /// Opens a window: a top-level window the program draws into through
    /// `framebuffer`, sized in logical units.
    pub fn window(&self, title: &str, width: i64, height: i64, resizable: bool) -> Result<Window> {
        self.window_with(&WindowOptions {
            title: title.to_string(),
            width,
            height,
            resizable,
            ..WindowOptions::default()
        })
    }

    /// Makes `width` x `height` RGBA pixels the application's icon: every
    /// open window takes it, and so does each window opened after.
    pub fn set_icon(&self, rgba: &[u8], width: i64, height: i64) -> Result<()> {
        support::require_main_thread("App::set_icon");
        window::set_app_icon(rgba, width, height)
    }

    /// Opens the window `options` describe.
    pub fn window_with(&self, options: &WindowOptions) -> Result<Window> {
        support::require_main_thread("App::window_with");
        window::create(options, self.event_base + event::REDRAW_OFFSET)
    }

    /// The open window `id` names.
    pub fn window_by_id(&self, id: WindowId) -> Option<Window> {
        support::require_main_thread("App::window_by_id");
        window::from_id(id, self.event_base + event::REDRAW_OFFSET)
    }

    /// Waits for the next event, without a timeout. The wait sits in the
    /// platform's own wait, so an idle loop takes no CPU, and hands the
    /// thread's freed memory back first.
    pub fn wait(&self) -> Result<Event> {
        support::require_main_thread("App::wait");
        let mut e = new_event();
        loop {
            let slice = idle::thread_idle().map_or(-1, idle::millis);
            let slice = support::c_int(slice, "a wait timeout")?;
            // SAFETY: e is valid storage SDL fills.
            if !hooks::pumping(|| unsafe { SDL_WaitEventTimeout(&mut e, slice) }) {
                if slice >= 0 {
                    continue;
                }
                return Err(support::failure("waiting for events"));
            }
            if let Some(event) = self.take(&e) {
                return Ok(event);
            }
        }
    }

    /// Waits up to `ms` milliseconds for the next event: `None` when none came.
    pub fn wait_timeout(&self, ms: i64) -> Result<Option<Event>> {
        support::require_main_thread("App::wait_timeout");
        let started = Instant::now();
        let mut e = new_event();
        loop {
            let left = ms - started.elapsed().as_millis() as i64;
            if left < 0 {
                return Ok(None);
            }
            let bound = idle::thread_idle().map_or(left, |due| idle::millis(due).min(left));
            let slice = support::c_int(bound, "a wait timeout")?;
            // SAFETY: e is valid storage SDL fills.
            if !hooks::pumping(|| unsafe { SDL_WaitEventTimeout(&mut e, slice) }) {
                if bound < left {
                    continue;
                }
                return Ok(None);
            }
            if let Some(event) = self.take(&e) {
                return Ok(Some(event));
            }
        }
    }

    /// The next event of the batch the platform has delivered, without
    /// waiting; `None` ends the batch.
    pub fn poll(&self) -> Option<Event> {
        support::require_main_thread("App::poll");
        let mut e = new_event();
        // SAFETY: e is valid storage SDL fills.
        while hooks::pumping(|| unsafe { SDL_PollEvent(&mut e) }) {
            if let Some(event) = self.take(&e) {
                return Some(event);
            }
        }
        None
    }

    fn take(&self, e: &SDL_Event) -> Option<Event> {
        let decoded = event::decode(e, self.event_base);
        simulate::release_injected(e);
        match &decoded {
            Some(Event::Wake) => self.pending_wake.store(false, Ordering::Release),
            Some(Event::ThemeChanged { .. }) => frame::reapply(),
            Some(Event::Moved { window, .. } | Event::Resized { window, .. }) => {
                access::moved(u64::from(window.0))
            }
            _ => {}
        }
        decoded
    }

    /// A waker other threads may hold to wake `wait`.
    pub fn waker(&self) -> Waker {
        Waker {
            wake_type: self.event_base + event::WAKE_OFFSET,
            pending: self.pending_wake.clone(),
        }
    }

    /// Calls `redraw` with the window being resized while the user drags its
    /// border, on Windows and macOS, where the system runs a loop of its own
    /// until the drag ends and `wait` does not return meanwhile. On macOS it
    /// also runs for each size change during a `wait` or `poll`, so the frame
    /// it paints shows together with the new size.
    pub fn on_live_resize(&mut self, redraw: Box<dyn Fn(Window) + Send>) -> Result<()> {
        support::require_main_thread("App::on_live_resize");
        self.clear_live_resize()?;
        hooks::add_live_resize(hooks::LiveResize {
            redraw,
            redraw_type: self.event_base + event::REDRAW_OFFSET,
        })
    }

    /// Runs `pump`, typically a `wait`, `wait_timeout` or `poll`, with
    /// `redraw` painting the window a live resize changes meanwhile, as
    /// `on_live_resize` does, but borrowing the caller's state for just this
    /// pump. It takes the place of the `on_live_resize` hook while it runs,
    /// and a redraw that pumps again starts no second redraw.
    pub fn redrawing<R>(&self, redraw: &mut dyn FnMut(Window), pump: impl FnOnce() -> R) -> R {
        support::require_main_thread("App::redrawing");
        hooks::redrawing(redraw, self.event_base + event::REDRAW_OFFSET, pump)
    }

    /// Removes the live-resize hook.
    pub fn clear_live_resize(&mut self) -> Result<()> {
        support::require_main_thread("App::clear_live_resize");
        hooks::remove_live_resize()
    }

    /// The modifier keys held now.
    pub fn mods(&self) -> Mods {
        support::require_main_thread("App::mods");
        // SAFETY: plain query.
        event::mods_from(unsafe { SDL_GetModState() })
    }

    /// Whether the key at `scancode` is held now.
    pub fn is_key_down(&self, scancode: Scancode) -> bool {
        support::require_main_thread("App::is_key_down");
        let code = i64::from(scancode.code());
        let mut count = 0;
        // SAFETY: SDL answers an array of `count` flags it owns.
        let state = unsafe { SDL_GetKeyboardState(&mut count) };
        if state.is_null() || code < 0 || code >= i64::from(count) {
            return false;
        }
        // SAFETY: code is within the array SDL answered.
        unsafe { *state.add(code as usize) }
    }

    /// The key the current layout puts at `scancode`.
    pub fn key_at(&self, scancode: Scancode) -> Key {
        support::require_main_thread("App::key_at");
        // SAFETY: plain query.
        Key::from_code(
            unsafe { SDL_GetKeyFromScancode(SDL_Scancode(scancode.code()), SDL_KMOD_NONE, false) }
                .0,
        )
    }

    /// A key's name for showing to the user (`"Return"`, `"A"`, `"Page Up"`).
    pub fn key_name(&self, key: Key) -> String {
        support::require_main_thread("App::key_name");
        // SAFETY: SDL answers a string it owns.
        support::read_text(unsafe { SDL_GetKeyName(SDL_Keycode(key.code())) })
    }

    /// The pointer's position over the focused window and the buttons held.
    pub fn pointer(&self) -> (f64, f64, Buttons) {
        support::require_main_thread("App::pointer");
        let (mut x, mut y) = (0f32, 0f32);
        // SAFETY: x and y are valid storage.
        let held = unsafe { SDL_GetMouseState(&mut x, &mut y) };
        (f64::from(x), f64::from(y), event::buttons_from(held))
    }

    /// Delivers pointer events while a button is held outside every window.
    pub fn capture_pointer(&self, captured: bool) -> Result<()> {
        support::require_main_thread("App::capture_pointer");
        // SAFETY: plain call.
        support::check(
            unsafe { SDL_CaptureMouse(captured) },
            "capturing the pointer",
        )
    }
}

/// The video driver in use (`"x11"`, `"wayland"`, `"windows"`, `"cocoa"`,
/// `"kmsdrm"`, `"offscreen"`), or "" before `init`.
pub fn video_driver() -> String {
    // SAFETY: SDL answers a string it owns, or null.
    support::read_text(unsafe { SDL_GetCurrentVideoDriver() })
}

/// The version of the SDL library underneath, as `major.minor.patch`.
pub fn sdl_version() -> String {
    let v = SDL_GetVersion();
    format!("{}.{}.{}", v / 1_000_000, (v / 1000) % 1000, v % 1000)
}
