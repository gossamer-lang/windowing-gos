//! Global shortcuts: key chords the desktop hands to the application while
//! any application has focus, such as the key that drops a terminal down.
//! A press arrives as `Event::Hotkey`. X11 grabs the key on the root window,
//! Windows and macOS register it with the system, and Wayland asks the
//! desktop portal, which may let the user confirm or change the chord; a
//! portal refusal arrives later as `Event::HotkeyFailed`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;

use crate::{App, Error, Key, Mods, Result, support, waker};

/// A global shortcut the application registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct HotkeyId(pub u32);

// The event types hotkey presses and portal refusals arrive as, set by `init`.
pub(crate) static HOTKEY_EVENT_TYPE: AtomicU32 = AtomicU32::new(0);

// Identities count from 1; Windows takes them as hot key ids below 0xC000.
static NEXT: AtomicU32 = AtomicU32::new(1);

// Why the portal refused a hotkey, by id, until its event is decoded.
static FAILURES: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

// `code` on a hotkey user event: a press, or a refusal whose reason
// `FAILURES` holds.
pub(crate) const PRESSED: i32 = 0;
pub(crate) const FAILED: i32 = 1;

/// Queues `Event::Hotkey` for hotkey `id`.
pub(crate) fn press(id: u32) -> Result<()> {
    waker::push_user_event(
        HOTKEY_EVENT_TYPE.load(Ordering::Acquire),
        0,
        PRESSED,
        u64::from(id),
        0,
    )
}

/// Queues `Event::HotkeyFailed` for hotkey `id` with `reason`.
pub(crate) fn refuse(id: u32, reason: String) -> Result<()> {
    FAILURES
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(id, reason);
    waker::push_user_event(
        HOTKEY_EVENT_TYPE.load(Ordering::Acquire),
        0,
        FAILED,
        u64::from(id),
        0,
    )
}

// The Wayland activation token the portal sent with the latest hotkey press,
// until a window spends it on taking focus.
#[cfg(free_unix)]
static ACTIVATION_TOKEN: Mutex<String> = Mutex::new(String::new());

#[cfg(free_unix)]
fn note_activation_token(token: String) {
    *ACTIVATION_TOKEN.lock() = token;
}

/// The Wayland activation token of the latest hotkey press, once; empty when
/// none is waiting.
#[cfg(free_unix)]
pub(crate) fn take_activation_token() -> String {
    std::mem::take(&mut *ACTIVATION_TOKEN.lock())
}

// A platform callback answers nothing, so a refused event is a lost press.
fn deliver(id: u32) {
    let _ = press(id);
}

#[cfg(free_unix)]
fn fail_later(id: u32, reason: String) {
    let _ = refuse(id, reason);
}

/// The reason the portal gave for refusing hotkey `id`, once.
pub(crate) fn take_failure(id: u32) -> String {
    FAILURES
        .lock()
        .as_mut()
        .and_then(|m| m.remove(&id))
        .unwrap_or_default()
}

/// The chord as people write it, such as "Ctrl+Shift+F12".
pub fn label(key: Key, mods: Mods) -> String {
    let mut parts: Vec<String> = Vec::new();
    if mods.ctrl {
        parts.push("Ctrl".into());
    }
    if mods.alt {
        parts.push(
            if cfg!(target_os = "macos") {
                "Option"
            } else {
                "Alt"
            }
            .into(),
        );
    }
    if mods.shift {
        parts.push("Shift".into());
    }
    if mods.gui {
        parts.push(
            if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Super"
            }
            .into(),
        );
    }
    parts.push(match key {
        Key::Char(c) => c.to_uppercase().to_string(),
        other => format!("{other:?}"),
    });
    parts.join("+")
}

impl App {
    /// Asks the desktop to deliver `key` pressed with exactly `mods` as
    /// `Event::Hotkey` while any application has focus. `description` names
    /// the action where the desktop lists shortcuts.
    pub fn register_hotkey(&self, key: Key, mods: Mods, description: &str) -> Result<HotkeyId> {
        support::require_main_thread("App::register_hotkey");
        let id = HotkeyId(NEXT.fetch_add(1, Ordering::Relaxed));
        platform::register(id.0, key, mods, description)?;
        Ok(id)
    }

    /// Gives hotkey `id` back to the desktop.
    pub fn unregister_hotkey(&self, id: HotkeyId) {
        support::require_main_thread("App::unregister_hotkey");
        platform::unregister(id.0)
    }
}

/// Gives every hotkey back as the session ends.
pub(crate) fn shutdown() {
    platform::shutdown();
}

#[cfg(free_unix)]
pub(crate) fn on_x11_event(raw: *mut sdl3_sys::system::XEvent) -> bool {
    platform::x11::on_event(raw)
}

#[cfg(target_os = "windows")]
pub(crate) fn on_windows_message(m: &windows_sys::Win32::UI::WindowsAndMessaging::MSG) {
    platform::on_message(m);
}

// The X keysym and its XKB name for `key`, as X11 and the portal spell it.
#[cfg(any(free_unix, test))]
fn keysym(key: Key) -> Option<(u64, String)> {
    let named = |value: u64, name: &str| Some((value, name.to_string()));
    match key {
        Key::A
        | Key::B
        | Key::C
        | Key::D
        | Key::E
        | Key::F
        | Key::G
        | Key::H
        | Key::I
        | Key::J
        | Key::K
        | Key::L
        | Key::M
        | Key::N
        | Key::O
        | Key::P
        | Key::Q
        | Key::R
        | Key::S
        | Key::T
        | Key::U
        | Key::V
        | Key::W
        | Key::X
        | Key::Y
        | Key::Z => {
            let letter = format!("{key:?}").to_lowercase();
            let c = letter.chars().next()?;
            Some((u64::from(c), letter))
        }
        Key::Num0
        | Key::Num1
        | Key::Num2
        | Key::Num3
        | Key::Num4
        | Key::Num5
        | Key::Num6
        | Key::Num7
        | Key::Num8
        | Key::Num9 => {
            let digit = format!("{key:?}").trim_start_matches("Num").to_string();
            let c = digit.chars().next()?;
            Some((u64::from(c), digit))
        }
        Key::F1
        | Key::F2
        | Key::F3
        | Key::F4
        | Key::F5
        | Key::F6
        | Key::F7
        | Key::F8
        | Key::F9
        | Key::F10
        | Key::F11
        | Key::F12
        | Key::F13
        | Key::F14
        | Key::F15
        | Key::F16
        | Key::F17
        | Key::F18
        | Key::F19
        | Key::F20
        | Key::F21
        | Key::F22
        | Key::F23
        | Key::F24 => {
            let name = format!("{key:?}");
            let n: u64 = name[1..].parse().ok()?;
            Some((0xFFBE + n - 1, name))
        }
        Key::Space => named(0x20, "space"),
        Key::Return => named(0xFF0D, "Return"),
        Key::Tab => named(0xFF09, "Tab"),
        Key::Escape => named(0xFF1B, "Escape"),
        Key::Backspace => named(0xFF08, "BackSpace"),
        Key::Insert => named(0xFF63, "Insert"),
        Key::Delete => named(0xFFFF, "Delete"),
        Key::Home => named(0xFF50, "Home"),
        Key::End => named(0xFF57, "End"),
        Key::PageUp => named(0xFF55, "Prior"),
        Key::PageDown => named(0xFF56, "Next"),
        Key::Left => named(0xFF51, "Left"),
        Key::Up => named(0xFF52, "Up"),
        Key::Right => named(0xFF53, "Right"),
        Key::Down => named(0xFF54, "Down"),
        Key::Pause => named(0xFF13, "Pause"),
        Key::ScrollLock => named(0xFF14, "Scroll_Lock"),
        Key::PrintScreen => named(0xFF61, "Print"),
        Key::Char(c) => {
            let name = match c {
                '`' => "grave",
                '-' => "minus",
                '=' => "equal",
                '[' => "bracketleft",
                ']' => "bracketright",
                '\\' => "backslash",
                ';' => "semicolon",
                '\'' => "apostrophe",
                ',' => "comma",
                '.' => "period",
                '/' => "slash",
                _ => return None,
            };
            Some((u64::from(c), name.to_string()))
        }
        _ => None,
    }
}

fn unsupported_key(key: Key) -> Error {
    Error::new(format!("{key:?} can't be a global shortcut"))
}

#[cfg(free_unix)]
mod platform {
    use super::{Error, Key, Mods, Result};

    pub(super) fn register(id: u32, key: Key, mods: Mods, description: &str) -> Result<()> {
        match crate::video_driver().as_str() {
            "x11" => x11::register(id, key, mods),
            "wayland" => portal::register(id, key, mods, description),
            other => Err(Error::new(format!(
                "global shortcuts are unavailable with the {other} video driver"
            ))),
        }
    }

    pub(super) fn unregister(id: u32) {
        x11::unregister(id);
        portal::unregister(id);
    }

    pub(super) fn shutdown() {
        x11::shutdown();
        portal::shutdown();
    }

    pub(super) mod x11 {
        use std::ffi::c_int;
        use std::sync::atomic::{AtomicBool, Ordering};

        use parking_lot::Mutex;
        use sdl3_sys::properties::SDL_GetPointerProperty;
        use sdl3_sys::stdinc::SDL_free;
        use sdl3_sys::system::XEvent as SdlXEvent;
        use sdl3_sys::video::{
            SDL_GetWindowProperties, SDL_GetWindows, SDL_PROP_WINDOW_X11_DISPLAY_POINTER,
        };
        use x11_dl::xlib::{self, Display, XErrorEvent, XEvent};

        use crate::hooks::x11_events;
        use crate::{Error, Key, Mods, Result};

        // NumLock and CapsLock change the state a press carries, so each
        // chord is grabbed under every combination of the two.
        const LOCKS: [u32; 4] = [
            0,
            xlib::LockMask,
            xlib::Mod2Mask,
            xlib::LockMask | xlib::Mod2Mask,
        ];
        const MODIFIERS: u32 =
            xlib::ShiftMask | xlib::ControlMask | xlib::Mod1Mask | xlib::Mod4Mask;
        // X's BadAccess: another client holds the grab.
        const BAD_ACCESS: u8 = 10;

        struct Grab {
            id: u32,
            display: usize,
            keycode: u32,
            state: u32,
            held: bool,
        }

        // Only the main thread touches the grabs: SDL runs the X11 hook in
        // its event pump.
        static GRABS: Mutex<Vec<Grab>> = Mutex::new(Vec::new());
        static REFUSED: AtomicBool = AtomicBool::new(false);

        unsafe extern "C" fn on_error(_display: *mut Display, error: *mut XErrorEvent) -> c_int {
            // SAFETY: Xlib hands a complete error event.
            if unsafe { (*error).error_code } == BAD_ACCESS {
                REFUSED.store(true, Ordering::Release);
            }
            0
        }

        // SDL's X connection, read from any open window.
        fn display() -> Result<*mut Display> {
            let mut count = 0;
            // SAFETY: SDL answers an array it allocated, or null.
            let windows = unsafe { SDL_GetWindows(&mut count) };
            if windows.is_null() {
                return Err(Error::new("a global shortcut needs an open window"));
            }
            // SAFETY: windows holds `count` live window pointers.
            let display = (count > 0).then(|| unsafe {
                SDL_GetPointerProperty(
                    SDL_GetWindowProperties(*windows),
                    SDL_PROP_WINDOW_X11_DISPLAY_POINTER,
                    std::ptr::null_mut(),
                )
            });
            // SAFETY: frees the array SDL allocated.
            unsafe { SDL_free(windows.cast()) };
            match display {
                Some(d) if !d.is_null() => Ok(d.cast()),
                _ => Err(Error::new("a global shortcut needs an open X11 window")),
            }
        }

        fn state_of(mods: Mods) -> u32 {
            let mut state = 0;
            if mods.shift {
                state |= xlib::ShiftMask;
            }
            if mods.ctrl {
                state |= xlib::ControlMask;
            }
            if mods.alt {
                state |= xlib::Mod1Mask;
            }
            if mods.gui {
                state |= xlib::Mod4Mask;
            }
            state
        }

        fn grab_all(x: &xlib::Xlib, display: *mut Display, keycode: u32, state: u32, grab: bool) {
            // SAFETY: display is SDL's live connection on the main thread.
            unsafe {
                let root = (x.XDefaultRootWindow)(display);
                for lock in LOCKS {
                    if grab {
                        (x.XGrabKey)(
                            display,
                            keycode as c_int,
                            state | lock,
                            root,
                            0,
                            xlib::GrabModeAsync,
                            xlib::GrabModeAsync,
                        );
                    } else {
                        (x.XUngrabKey)(display, keycode as c_int, state | lock, root);
                    }
                }
            }
        }

        pub(crate) fn register(id: u32, key: Key, mods: Mods) -> Result<()> {
            let x = crate::xlib::xlib().ok_or_else(|| Error::new("Xlib is unavailable"))?;
            let display = display()?;
            let (sym, _) =
                super::super::keysym(key).ok_or_else(|| super::super::unsupported_key(key))?;
            // SAFETY: display is live; a keysym is a plain number.
            let keycode = u32::from(unsafe { (x.XKeysymToKeycode)(display, sym) });
            if keycode == 0 {
                return Err(Error::new(format!("no key on this keyboard types {key:?}")));
            }
            let state = state_of(mods);
            // A refused grab is reported asynchronously, so the grabs run
            // under a handler that notes BadAccess, flushed by the sync.
            REFUSED.store(false, Ordering::Release);
            // SAFETY: the handler is swapped on the main thread around a sync.
            let previous = unsafe { (x.XSetErrorHandler)(Some(on_error)) };
            grab_all(x, display, keycode, state, true);
            // SAFETY: display is live.
            unsafe {
                (x.XSync)(display, 0);
                (x.XSetErrorHandler)(previous);
            }
            if REFUSED.load(Ordering::Acquire) {
                grab_all(x, display, keycode, state, false);
                // SAFETY: display is live.
                unsafe { (x.XFlush)(display) };
                return Err(Error::new(format!(
                    "another application already uses {}",
                    super::super::label(key, mods)
                )));
            }
            GRABS.lock().push(Grab {
                id,
                display: display as usize,
                keycode,
                state,
                held: false,
            });
            x11_events::subscribe(x11_events::HOTKEYS);
            Ok(())
        }

        fn release(grab: &Grab) {
            if let Some(x) = crate::xlib::xlib() {
                let display = grab.display as *mut Display;
                grab_all(x, display, grab.keycode, grab.state, false);
                // SAFETY: the connection lives until SDL quits, after shutdown.
                unsafe { (x.XFlush)(display) };
            }
        }

        pub(crate) fn unregister(id: u32) {
            let mut grabs = GRABS.lock();
            let display = grabs.first().map(|g| g.display);
            grabs.retain(|g| {
                if g.id == id {
                    release(g);
                }
                g.id != id
            });
            if let Some(display) = display
                && !grabs.iter().any(|g| g.held)
            {
                releases::watch(display as *mut Display, false);
            }
            if grabs.is_empty() {
                x11_events::unsubscribe(x11_events::HOTKEYS);
            }
        }

        pub(crate) fn shutdown() {
            let mut grabs = GRABS.lock();
            for g in grabs.iter() {
                release(g);
            }
            if !grabs.is_empty() {
                x11_events::unsubscribe(x11_events::HOTKEYS);
            }
            grabs.clear();
        }

        // A grabbed chord's press answers false, so SDL leaves it alone. A
        // held key repeats as further presses, which are skipped until its
        // release.
        pub(crate) fn on_event(raw: *mut SdlXEvent) -> bool {
            // SAFETY: SDL hands a complete Xlib event.
            let event = unsafe { &*raw.cast::<XEvent>() };
            match event.get_type() {
                xlib::KeyPress => {
                    // SAFETY: a key event's union member is `key`.
                    let key = unsafe { event.key };
                    let mut grabs = GRABS.lock();
                    let Some(grab) = grabs
                        .iter_mut()
                        .find(|g| g.keycode == key.keycode && g.state == key.state & MODIFIERS)
                    else {
                        return true;
                    };
                    if !grab.held {
                        grab.held = true;
                        crate::xlib::note_outside_input(key.time);
                        releases::watch(grab.display as *mut Display, true);
                        super::super::deliver(grab.id);
                    }
                    false
                }
                // The chord's modifiers may be let go first, so a release
                // answers to its key alone.
                xlib::KeyRelease => {
                    // SAFETY: a key event's union member is `key`.
                    !released(unsafe { event.key }.keycode)
                }
                xlib::GenericEvent => {
                    // SAFETY: SDL fetched the cookie's data before the hook.
                    let cookie = unsafe { event.generic_event_cookie };
                    if let Some(keycode) = releases::keycode(&cookie) {
                        released(keycode);
                    }
                    true
                }
                _ => true,
            }
        }

        // Ends the hold on every chord of `keycode`, answering whether one
        // was held.
        fn released(keycode: u32) -> bool {
            let mut grabs = GRABS.lock();
            let mut ended = false;
            for g in grabs.iter_mut().filter(|g| g.keycode == keycode && g.held) {
                g.held = false;
                ended = true;
            }
            if ended
                && let Some(g) = grabs.first()
                && !grabs.iter().any(|g| g.held)
            {
                releases::watch(g.display as *mut Display, false);
            }
            ended
        }

        // SDL gives the keyboard back (XUngrabKeyboard) whenever one of its
        // windows gains or loses focus, which also ends the grab a chord's
        // press activated, so the key's core release may go to another
        // client. XInput2's raw key releases reach the root window whatever
        // the focus or grab, so while a chord is held this client also takes
        // those, added to SDL's own selection on the root.
        mod releases {
            use std::ffi::{CString, c_int};
            use std::sync::OnceLock;
            use std::sync::atomic::{AtomicI32, Ordering};

            use x11_dl::xinput2::{
                self, XI_LASTEVENT, XI_RawKeyRelease, XIAllMasterDevices, XIEventMask, XIRawEvent,
                XInput2,
            };
            use x11_dl::xlib::{Display, XErrorEvent, XGenericEventCookie};

            // The XInput2 major opcode while raw key releases are selected; 0
            // otherwise.
            static WATCHING: AtomicI32 = AtomicI32::new(0);

            fn xinput2() -> Option<&'static XInput2> {
                static XI: OnceLock<Option<XInput2>> = OnceLock::new();
                XI.get_or_init(|| XInput2::open().ok()).as_ref()
            }

            fn opcode(x: &x11_dl::xlib::Xlib, display: *mut Display) -> c_int {
                let name = CString::new("XInputExtension").expect("a literal without NUL");
                let (mut opcode, mut event, mut error) = (0, 0, 0);
                // SAFETY: display is live; the outputs are valid storage.
                let present = unsafe {
                    (x.XQueryExtension)(display, name.as_ptr(), &mut opcode, &mut event, &mut error)
                };
                if present == 0 { 0 } else { opcode }
            }

            unsafe extern "C" fn ignore(_display: *mut Display, _error: *mut XErrorEvent) -> c_int {
                0
            }

            /// Adds raw key releases to this client's root selection, or
            /// takes them out again.
            pub(super) fn watch(display: *mut Display, on: bool) {
                if (WATCHING.load(Ordering::Acquire) != 0) == on {
                    return;
                }
                let (Some(x), Some(xi)) = (crate::xlib::xlib(), xinput2()) else {
                    return;
                };
                let opcode = opcode(x, display);
                if opcode == 0 {
                    return;
                }
                // SAFETY: display is SDL's live connection on the main thread;
                // the selection SDL made is read, copied and freed here.
                unsafe {
                    let root = (x.XDefaultRootWindow)(display);
                    let mut mask = vec![0u8; (XI_LASTEVENT as usize >> 3) + 1];
                    let mut count = 0;
                    let selected = (xi.XIGetSelectedEvents)(display, root, &mut count);
                    if !selected.is_null() {
                        for m in std::slice::from_raw_parts(selected, count.max(0) as usize) {
                            if m.deviceid == XIAllMasterDevices && !m.mask.is_null() {
                                let theirs =
                                    std::slice::from_raw_parts(m.mask, m.mask_len.max(0) as usize);
                                if theirs.len() > mask.len() {
                                    mask.resize(theirs.len(), 0);
                                }
                                for (ours, bits) in mask.iter_mut().zip(theirs) {
                                    *ours |= bits;
                                }
                            }
                        }
                        (x.XFree)(selected.cast());
                    }
                    let byte = XI_RawKeyRelease as usize >> 3;
                    let bit = 1u8 << (XI_RawKeyRelease & 7);
                    if on {
                        xinput2::XISetMask(&mut mask, XI_RawKeyRelease);
                    } else {
                        mask[byte] &= !bit;
                    }
                    let mut selection = XIEventMask {
                        deviceid: XIAllMasterDevices,
                        mask_len: mask.len() as c_int,
                        mask: mask.as_mut_ptr(),
                    };
                    // A server without XInput2 2.0 refuses the selection; that
                    // leaves the core release as the only one.
                    let previous = (x.XSetErrorHandler)(Some(ignore));
                    (xi.XISelectEvents)(display, root, &mut selection, 1);
                    (x.XSync)(display, 0);
                    (x.XSetErrorHandler)(previous);
                }
                WATCHING.store(if on { opcode } else { 0 }, Ordering::Release);
            }

            /// The key a raw key release lets go of.
            pub(super) fn keycode(cookie: &XGenericEventCookie) -> Option<u32> {
                let opcode = WATCHING.load(Ordering::Acquire);
                if opcode == 0
                    || cookie.extension != opcode
                    || cookie.evtype != XI_RawKeyRelease
                    || cookie.data.is_null()
                {
                    return None;
                }
                // SAFETY: an XI_RawKeyRelease cookie's data is an XIRawEvent.
                let raw = unsafe { &*cookie.data.cast::<XIRawEvent>() };
                u32::try_from(raw.detail).ok()
            }
        }
    }

    // The desktop portal's GlobalShortcuts: one session per hotkey, bound
    // and watched on a thread of its own, since binding may wait on a dialog.
    mod portal {
        use std::collections::HashMap;

        use parking_lot::Mutex;
        use zbus::blocking::{Connection, MessageIterator};
        use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

        use crate::{Error, Key, Mods, Result};

        const DESTINATION: &str = "org.freedesktop.portal.Desktop";
        const PATH: &str = "/org/freedesktop/portal/desktop";
        const INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

        // The session each hotkey's thread opened, once it has one.
        static SESSIONS: Mutex<Vec<(u32, OwnedObjectPath)>> = Mutex::new(Vec::new());

        fn trigger(key: Key, mods: Mods) -> Option<String> {
            let (_, name) = super::super::keysym(key)?;
            let mut parts = Vec::new();
            if mods.ctrl {
                parts.push("CTRL".to_string());
            }
            if mods.alt {
                parts.push("ALT".to_string());
            }
            if mods.shift {
                parts.push("SHIFT".to_string());
            }
            if mods.gui {
                parts.push("LOGO".to_string());
            }
            parts.push(name);
            Some(parts.join("+"))
        }

        pub(crate) fn register(id: u32, key: Key, mods: Mods, description: &str) -> Result<()> {
            let connection = crate::bus::session()
                .ok_or_else(|| Error::new("global shortcuts need the session bus"))?;
            let preferred = trigger(key, mods).ok_or_else(|| super::super::unsupported_key(key))?;
            let description = description.to_string();
            std::thread::Builder::new()
                .name("windowing-hotkey".into())
                .spawn(move || {
                    if let Err(e) = watch(&connection, id, &preferred, &description) {
                        super::super::fail_later(id, e.to_string());
                    }
                })
                .map_err(|e| Error::new(format!("starting the hotkey thread: {e}")))?;
            Ok(())
        }

        // The object path the portal answers a request with `token` on.
        fn request_path(connection: &Connection, token: &str) -> Result<String> {
            let sender = connection
                .unique_name()
                .ok_or_else(|| Error::new("the session bus gave no name"))?
                .as_str()
                .trim_start_matches(':')
                .replace('.', "_");
            Ok(format!("{PATH}/request/{sender}/{token}"))
        }

        // Calls `method`, whose answer arrives as the Response signal of a
        // Request object, and answers its results.
        fn request<B>(
            connection: &Connection,
            token: &str,
            method: &str,
            body: &B,
        ) -> Result<HashMap<String, OwnedValue>>
        where
            B: serde::Serialize + zbus::zvariant::DynamicType,
        {
            let path = request_path(connection, token)?;
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.portal.Request")
                .and_then(|b| b.member("Response"))
                .and_then(|b| b.path(path.as_str()))
                .map(|b| b.build())
                .map_err(|e| Error::new(e.to_string()))?;
            // Subscribing before the call keeps a quick answer from slipping past.
            let mut answers = MessageIterator::for_match_rule(rule, connection, Some(4))
                .map_err(|e| Error::new(e.to_string()))?;
            connection
                .call_method(Some(DESTINATION), PATH, Some(INTERFACE), method, body)
                .map_err(|e| Error::new(e.to_string()))?;
            let answer = answers
                .next()
                .ok_or_else(|| Error::new("the portal closed without answering"))?
                .map_err(|e| Error::new(e.to_string()))?;
            let (code, results): (u32, HashMap<String, OwnedValue>) =
                answer
                    .body()
                    .deserialize()
                    .map_err(|e| Error::new(e.to_string()))?;
            match code {
                0 => Ok(results),
                1 => Err(Error::new("the shortcut was declined")),
                _ => Err(Error::new("the desktop refused the shortcut")),
            }
        }

        fn watch(
            connection: &Connection,
            id: u32,
            preferred: &str,
            description: &str,
        ) -> Result<()> {
            let shortcut = format!("hotkey-{id}");
            let mut options: HashMap<&str, Value> = HashMap::new();
            options.insert(
                "handle_token",
                Value::from(format!("windowing_hk_create_{id}")),
            );
            options.insert(
                "session_handle_token",
                Value::from(format!("windowing_hk_{id}")),
            );
            let created = request(
                connection,
                &format!("windowing_hk_create_{id}"),
                "CreateSession",
                &(options,),
            )?;
            let session: OwnedObjectPath = created
                .get("session_handle")
                .and_then(|v| String::try_from(v.clone()).ok())
                .and_then(|s| OwnedObjectPath::try_from(s).ok())
                .ok_or_else(|| Error::new("the portal opened no session"))?;
            let activations = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(INTERFACE)
                .and_then(|b| b.member("Activated"))
                .map(|b| b.build())
                .map_err(|e| Error::new(e.to_string()))?;
            let presses = MessageIterator::for_match_rule(activations, connection, Some(16))
                .map_err(|e| Error::new(e.to_string()))?;
            let mut properties: HashMap<&str, Value> = HashMap::new();
            properties.insert("description", Value::from(description));
            properties.insert("preferred_trigger", Value::from(preferred));
            let mut bind: HashMap<&str, Value> = HashMap::new();
            bind.insert(
                "handle_token",
                Value::from(format!("windowing_hk_bind_{id}")),
            );
            request(
                connection,
                &format!("windowing_hk_bind_{id}"),
                "BindShortcuts",
                &(&session, vec![(shortcut.as_str(), properties)], "", bind),
            )?;
            SESSIONS.lock().push((id, session.clone()));
            for message in presses {
                let Ok(message) = message else { break };
                let Ok((from, name, _time, options)) =
                    message
                        .body()
                        .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>(
                        )
                else {
                    continue;
                };
                if from == session && name == shortcut {
                    if let Some(token) = options
                        .get("activation_token")
                        .and_then(|t| String::try_from(t.clone()).ok())
                    {
                        super::super::note_activation_token(token);
                    }
                    super::super::deliver(id);
                }
                if !SESSIONS.lock().iter().any(|(held, _)| *held == id) {
                    break;
                }
            }
            Ok(())
        }

        fn close(session: &OwnedObjectPath) {
            if let Some(connection) = crate::bus::session() {
                let _ = connection.call_method(
                    Some(DESTINATION),
                    session.as_str(),
                    Some("org.freedesktop.portal.Session"),
                    "Close",
                    &(),
                );
            }
        }

        pub(crate) fn unregister(id: u32) {
            let mut sessions = SESSIONS.lock();
            sessions.retain(|(held, session)| {
                if *held == id {
                    close(session);
                }
                *held != id
            });
        }

        pub(crate) fn shutdown() {
            for (_, session) in SESSIONS.lock().drain(..) {
                close(&session);
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
        VkKeyScanW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, WM_HOTKEY};

    use parking_lot::Mutex;

    use crate::hooks::windows_messages;
    use crate::{Error, Key, Mods, Result};

    static REGISTERED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    fn virtual_key(key: Key) -> Option<u32> {
        let name = format!("{key:?}");
        let code = match key {
            Key::A
            | Key::B
            | Key::C
            | Key::D
            | Key::E
            | Key::F
            | Key::G
            | Key::H
            | Key::I
            | Key::J
            | Key::K
            | Key::L
            | Key::M
            | Key::N
            | Key::O
            | Key::P
            | Key::Q
            | Key::R
            | Key::S
            | Key::T
            | Key::U
            | Key::V
            | Key::W
            | Key::X
            | Key::Y
            | Key::Z => u32::from(name.as_bytes()[0]),
            Key::Num0
            | Key::Num1
            | Key::Num2
            | Key::Num3
            | Key::Num4
            | Key::Num5
            | Key::Num6
            | Key::Num7
            | Key::Num8
            | Key::Num9 => u32::from(name.as_bytes()[3]),
            Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
            | Key::F13
            | Key::F14
            | Key::F15
            | Key::F16
            | Key::F17
            | Key::F18
            | Key::F19
            | Key::F20
            | Key::F21
            | Key::F22
            | Key::F23
            | Key::F24 => 0x70 + name[1..].parse::<u32>().ok()? - 1,
            Key::Space => 0x20,
            Key::Return => 0x0D,
            Key::Tab => 0x09,
            Key::Escape => 0x1B,
            Key::Backspace => 0x08,
            Key::Insert => 0x2D,
            Key::Delete => 0x2E,
            Key::Home => 0x24,
            Key::End => 0x23,
            Key::PageUp => 0x21,
            Key::PageDown => 0x22,
            Key::Left => 0x25,
            Key::Up => 0x26,
            Key::Right => 0x27,
            Key::Down => 0x28,
            Key::Pause => 0x13,
            Key::ScrollLock => 0x91,
            Key::PrintScreen => 0x2C,
            Key::Char(c) if (c as u32) < 0x10000 => {
                // SAFETY: a plain lookup in the current keyboard layout.
                let scan = unsafe { VkKeyScanW(c as u16) };
                if scan == -1 {
                    return None;
                }
                u32::from(scan as u16 & 0xFF)
            }
            _ => return None,
        };
        Some(code)
    }

    pub(super) fn register(id: u32, key: Key, mods: Mods, _description: &str) -> Result<()> {
        let vk = virtual_key(key).ok_or_else(|| super::unsupported_key(key))?;
        let mut flags = MOD_NOREPEAT;
        if mods.ctrl {
            flags |= MOD_CONTROL;
        }
        if mods.alt {
            flags |= MOD_ALT;
        }
        if mods.shift {
            flags |= MOD_SHIFT;
        }
        if mods.gui {
            flags |= MOD_WIN;
        }
        // A null window posts WM_HOTKEY to this thread's queue, which SDL pumps.
        // SAFETY: plain registration on the main thread.
        if unsafe { RegisterHotKey(std::ptr::null_mut(), id as i32, flags, vk) } == 0 {
            return Err(Error::new(format!(
                "another application already uses {}",
                super::label(key, mods)
            )));
        }
        REGISTERED.lock().push(id);
        windows_messages::subscribe(windows_messages::HOTKEYS);
        Ok(())
    }

    pub(super) fn unregister(id: u32) {
        let mut registered = REGISTERED.lock();
        if registered.contains(&id) {
            // SAFETY: undoes this thread's registration.
            unsafe { UnregisterHotKey(std::ptr::null_mut(), id as i32) };
            registered.retain(|held| *held != id);
        }
        if registered.is_empty() {
            windows_messages::unsubscribe(windows_messages::HOTKEYS);
        }
    }

    pub(super) fn shutdown() {
        let ids: Vec<u32> = REGISTERED.lock().clone();
        for id in ids {
            unregister(id);
        }
    }

    pub(super) fn on_message(m: &MSG) {
        if m.message == WM_HOTKEY && REGISTERED.lock().contains(&(m.wParam as u32)) {
            super::deliver(m.wParam as u32);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    use parking_lot::Mutex;

    use crate::{Error, Key, Mods, Result};

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    type Handler = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> *mut c_void;
        fn InstallEventHandler(
            target: *mut c_void,
            handler: Handler,
            count: u32,
            types: *const EventTypeSpec,
            user_data: *mut c_void,
            out: *mut *mut c_void,
        ) -> i32;
        fn RegisterEventHotKey(
            key_code: u32,
            modifiers: u32,
            id: EventHotKeyId,
            target: *mut c_void,
            options: u32,
            out: *mut *mut c_void,
        ) -> i32;
        fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
        fn GetEventParameter(
            event: *mut c_void,
            name: u32,
            desired: u32,
            actual: *mut u32,
            size: usize,
            actual_size: *mut usize,
            data: *mut c_void,
        ) -> i32;
    }

    const CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
    const HOT_KEY_PRESSED: u32 = 5;
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
    const TYPE_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
    const SIGNATURE: u32 = u32::from_be_bytes(*b"wndw");
    const CMD: u32 = 1 << 8;
    const SHIFT: u32 = 1 << 9;
    const OPTION: u32 = 1 << 11;
    const CONTROL: u32 = 1 << 12;

    struct Registered {
        id: u32,
        handle: usize,
    }

    static REGISTERED: Mutex<Vec<Registered>> = Mutex::new(Vec::new());
    static HANDLER: Mutex<bool> = Mutex::new(false);

    unsafe extern "C" fn on_hot_key(
        _next: *mut c_void,
        event: *mut c_void,
        _data: *mut c_void,
    ) -> i32 {
        let mut pressed = EventHotKeyId::default();
        // SAFETY: event is the Carbon event being dispatched; pressed is valid storage.
        let read = unsafe {
            GetEventParameter(
                event,
                DIRECT_OBJECT,
                TYPE_HOT_KEY_ID,
                std::ptr::null_mut(),
                std::mem::size_of::<EventHotKeyId>(),
                std::ptr::null_mut(),
                (&mut pressed as *mut EventHotKeyId).cast(),
            )
        };
        if read == 0 && pressed.signature == SIGNATURE {
            super::deliver(pressed.id);
        }
        0
    }

    // The ANSI layout's virtual key codes (Carbon's kVK_ constants).
    fn key_code(key: Key) -> Option<u32> {
        let code = match key {
            Key::A => 0x00,
            Key::S => 0x01,
            Key::D => 0x02,
            Key::F => 0x03,
            Key::H => 0x04,
            Key::G => 0x05,
            Key::Z => 0x06,
            Key::X => 0x07,
            Key::C => 0x08,
            Key::V => 0x09,
            Key::B => 0x0B,
            Key::Q => 0x0C,
            Key::W => 0x0D,
            Key::E => 0x0E,
            Key::R => 0x0F,
            Key::Y => 0x10,
            Key::T => 0x11,
            Key::Num1 => 0x12,
            Key::Num2 => 0x13,
            Key::Num3 => 0x14,
            Key::Num4 => 0x15,
            Key::Num6 => 0x16,
            Key::Num5 => 0x17,
            Key::Num9 => 0x19,
            Key::Num7 => 0x1A,
            Key::Num8 => 0x1C,
            Key::Num0 => 0x1D,
            Key::O => 0x1F,
            Key::U => 0x20,
            Key::I => 0x22,
            Key::P => 0x23,
            Key::Return => 0x24,
            Key::L => 0x25,
            Key::J => 0x26,
            Key::K => 0x28,
            Key::N => 0x2D,
            Key::M => 0x2E,
            Key::Tab => 0x30,
            Key::Space => 0x31,
            Key::Backspace => 0x33,
            Key::Escape => 0x35,
            Key::F1 => 0x7A,
            Key::F2 => 0x78,
            Key::F3 => 0x63,
            Key::F4 => 0x76,
            Key::F5 => 0x60,
            Key::F6 => 0x61,
            Key::F7 => 0x62,
            Key::F8 => 0x64,
            Key::F9 => 0x65,
            Key::F10 => 0x6D,
            Key::F11 => 0x67,
            Key::F12 => 0x6F,
            Key::F13 => 0x69,
            Key::F14 => 0x6B,
            Key::F15 => 0x71,
            Key::F16 => 0x6A,
            Key::F17 => 0x40,
            Key::F18 => 0x4F,
            Key::F19 => 0x50,
            Key::F20 => 0x5A,
            Key::Insert => 0x72,
            Key::Home => 0x73,
            Key::PageUp => 0x74,
            Key::Delete => 0x75,
            Key::End => 0x77,
            Key::PageDown => 0x79,
            Key::Left => 0x7B,
            Key::Right => 0x7C,
            Key::Down => 0x7D,
            Key::Up => 0x7E,
            Key::Char('=') => 0x18,
            Key::Char('-') => 0x1B,
            Key::Char(']') => 0x1E,
            Key::Char('[') => 0x21,
            Key::Char('\'') => 0x27,
            Key::Char(';') => 0x29,
            Key::Char('\\') => 0x2A,
            Key::Char(',') => 0x2B,
            Key::Char('/') => 0x2C,
            Key::Char('.') => 0x2F,
            Key::Char('`') => 0x32,
            _ => return None,
        };
        Some(code)
    }

    pub(super) fn register(id: u32, key: Key, mods: Mods, _description: &str) -> Result<()> {
        let code = key_code(key).ok_or_else(|| super::unsupported_key(key))?;
        let mut flags = 0;
        if mods.gui {
            flags |= CMD;
        }
        if mods.shift {
            flags |= SHIFT;
        }
        if mods.alt {
            flags |= OPTION;
        }
        if mods.ctrl {
            flags |= CONTROL;
        }
        let mut installed = HANDLER.lock();
        if !*installed {
            let types = [EventTypeSpec {
                event_class: CLASS_KEYBOARD,
                event_kind: HOT_KEY_PRESSED,
            }];
            // SAFETY: the handler is a plain function that lives for the program.
            let status = unsafe {
                InstallEventHandler(
                    GetApplicationEventTarget(),
                    on_hot_key,
                    1,
                    types.as_ptr(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                return Err(Error::new(format!(
                    "installing the hot key handler failed ({status})"
                )));
            }
            *installed = true;
        }
        let mut handle = std::ptr::null_mut();
        // SAFETY: plain registration on the main thread; handle is valid storage.
        let status = unsafe {
            RegisterEventHotKey(
                code,
                flags,
                EventHotKeyId {
                    signature: SIGNATURE,
                    id,
                },
                GetApplicationEventTarget(),
                0,
                &mut handle,
            )
        };
        if status != 0 {
            return Err(Error::new(format!(
                "another application already uses {}",
                super::label(key, mods)
            )));
        }
        REGISTERED.lock().push(Registered {
            id,
            handle: handle as usize,
        });
        Ok(())
    }

    pub(super) fn unregister(id: u32) {
        REGISTERED.lock().retain(|r| {
            if r.id == id {
                // SAFETY: handle came from RegisterEventHotKey and is released once.
                unsafe { UnregisterEventHotKey(r.handle as *mut c_void) };
            }
            r.id != id
        });
    }

    pub(super) fn shutdown() {
        for r in REGISTERED.lock().drain(..) {
            // SAFETY: handle came from RegisterEventHotKey and is released once.
            unsafe { UnregisterEventHotKey(r.handle as *mut c_void) };
        }
    }
}

#[cfg(not(any(free_unix, target_os = "windows", target_os = "macos")))]
mod platform {
    use crate::{Error, Key, Mods, Result};

    pub(super) fn register(_id: u32, _key: Key, _mods: Mods, _description: &str) -> Result<()> {
        Err(Error::new(
            "global shortcuts are unavailable on this platform",
        ))
    }

    pub(super) fn unregister(_id: u32) {}

    pub(super) fn shutdown() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_keys_and_letters_take_their_x_keysyms_and_names() {
        assert_eq!(keysym(Key::F12), Some((0xFFC9, "F12".to_string())));
        assert_eq!(keysym(Key::F1), Some((0xFFBE, "F1".to_string())));
        assert_eq!(keysym(Key::Q), Some((0x71, "q".to_string())));
        assert_eq!(keysym(Key::Num7), Some((0x37, "7".to_string())));
        assert_eq!(keysym(Key::Char('`')), Some((0x60, "grave".to_string())));
        assert_eq!(keysym(Key::Char('x')), None);
    }

    #[test]
    fn a_label_names_its_modifiers_before_its_key() {
        let mods = Mods {
            ctrl: true,
            shift: true,
            ..Mods::default()
        };
        assert_eq!(label(Key::F12, Mods::default()), "F12");
        assert!(label(Key::F12, mods).ends_with("Shift+F12"));
    }
}
