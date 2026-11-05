//! The X11 client library, loaded on first use from the copy SDL loaded.

use std::ffi::CString;
use std::os::raw::c_long;
use std::sync::OnceLock;

use x11_dl::xlib::{self, Atom, Display, Xlib};

/// Xlib's functions, or `None` when the library is not installed.
pub(crate) fn xlib() -> Option<&'static Xlib> {
    static XLIB: OnceLock<Option<Xlib>> = OnceLock::new();
    XLIB.get_or_init(|| Xlib::open().ok()).as_ref()
}

/// The atom `name` names on `display`, created when missing; 0 for a name
/// holding a NUL.
pub(crate) fn atom(x: &Xlib, display: *mut Display, name: &str) -> Atom {
    let Ok(text) = CString::new(name) else {
        return 0;
    };
    // SAFETY: display is a live connection; text outlives the call.
    unsafe { (x.XInternAtom)(display, text.as_ptr(), 0) }
}

/// Carries the launcher's startup identity and X server timestamp into EWMH.
pub(crate) fn activate(
    display: *mut std::ffi::c_void,
    window: u64,
    screen: i64,
    startup_id: &str,
) -> crate::Result<()> {
    use x11_dl::xlib::{self, ClientMessageData, XClientMessageEvent, XEvent};
    let x = xlib().ok_or_else(|| crate::Error::new("Xlib is unavailable"))?;
    let display = display as *mut Display;
    let window = window as xlib::Window;
    let startup = atom(x, display, "_NET_STARTUP_ID");
    let utf8 = atom(x, display, "UTF8_STRING");
    // SAFETY: these are this live SDL window's X handles; the byte count is bounded.
    unsafe {
        (x.XChangeProperty)(
            display,
            window,
            startup,
            utf8,
            8,
            xlib::PropModeReplace,
            startup_id.as_ptr(),
            startup_id.len() as i32,
        );
    }
    if let Some(time) = startup_id
        .rsplit_once("_TIME")
        .and_then(|(_, value)| value.parse::<u32>().ok())
    {
        let user_time = atom(x, display, "_NET_WM_USER_TIME");
        let time = time as std::os::raw::c_ulong;
        let mut message = XClientMessageEvent {
            type_: xlib::ClientMessage,
            serial: 0,
            send_event: 1,
            display,
            window,
            message_type: atom(x, display, "_NET_ACTIVE_WINDOW"),
            format: 32,
            data: ClientMessageData::new(),
        };
        message.data.set_long(0, 1);
        message.data.set_long(1, time as std::os::raw::c_long);
        let mut event = XEvent {
            client_message: message,
        };
        // SAFETY: Xlib consumes one native unsigned long for a format-32 property.
        unsafe {
            (x.XChangeProperty)(
                display,
                window,
                user_time,
                xlib::XA_CARDINAL,
                32,
                xlib::PropModeReplace,
                (&time as *const std::os::raw::c_ulong).cast(),
                1,
            );
            (x.XSendEvent)(
                display,
                (x.XRootWindow)(display, screen as i32),
                0,
                xlib::SubstructureNotifyMask | xlib::SubstructureRedirectMask,
                &mut event,
            );
        }
    }
    // SAFETY: display is the live SDL connection.
    unsafe {
        (x.XFlush)(display);
    }
    Ok(())
}

// The X server time of the latest key press the application took outside its
// own windows, such as a hotkey grabbed on the root window; 0 before any.
static OUTSIDE_INPUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Records the server time of input that reached the application outside its
/// windows, which entitles it to take focus at the window manager.
pub(crate) fn note_outside_input(time: std::os::raw::c_ulong) {
    OUTSIDE_INPUT.store(u64::from(time as u32), std::sync::atomic::Ordering::Relaxed);
}

// The shortcut press whose time a focus request last spent; 0 for none.
static SPENT_INPUT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether a focus request claiming `time` is a direct user action: a global
/// shortcut press newer than any input on the application's own windows,
/// not spent by an earlier request. Each press entitles one.
fn shortcut_entitles(time: u32) -> bool {
    use std::sync::atomic::Ordering;
    let outside = OUTSIDE_INPUT.load(Ordering::Relaxed) as u32;
    outside != 0
        && time == outside
        && SPENT_INPUT.swap(u64::from(outside), Ordering::Relaxed) as u32 != outside
}

// X server times are 32-bit milliseconds that wrap; the later of two is the
// one less than half the range ahead. 0 means none.
fn later(a: u32, b: u32) -> u32 {
    if b == 0 || (a != 0 && (a.wrapping_sub(b) as i32) > 0) {
        a
    } else {
        b
    }
}

// The window's last user time (_NET_WM_USER_TIME), 0 when unset.
fn user_time(x: &Xlib, display: *mut Display, window: xlib::Window) -> u32 {
    let mut kind: Atom = 0;
    let mut format = 0;
    let mut count: std::os::raw::c_ulong = 0;
    let mut after: std::os::raw::c_ulong = 0;
    let mut data: *mut u8 = std::ptr::null_mut();
    // SAFETY: display and window are live; Xlib allocates `data`, freed below.
    let read = unsafe {
        (x.XGetWindowProperty)(
            display,
            window,
            atom(x, display, "_NET_WM_USER_TIME"),
            0,
            1,
            0,
            xlib::XA_CARDINAL,
            &mut kind,
            &mut format,
            &mut count,
            &mut after,
            &mut data,
        )
    };
    let mut time = 0;
    if read == 0 && !data.is_null() {
        if format == 32 && count == 1 {
            // SAFETY: a format-32 property of one item is one native unsigned long.
            time = unsafe { *data.cast::<std::os::raw::c_ulong>() } as u32;
        }
        // SAFETY: data came from XGetWindowProperty.
        unsafe { (x.XFree)(data.cast()) };
    }
    time
}

// Gives the window the latest user time the application can claim: the
// latest input any of its windows (`siblings` included) or a hotkey took;
// 0 when there was none.
fn claim_user_time(x: &Xlib, display: *mut Display, window: xlib::Window, siblings: &[u64]) -> u32 {
    let own = user_time(x, display, window);
    let time = siblings.iter().fold(
        later(
            OUTSIDE_INPUT.load(std::sync::atomic::Ordering::Relaxed) as u32,
            own,
        ),
        |latest, &s| later(user_time(x, display, s as xlib::Window), latest),
    );
    if time != own {
        let value = std::os::raw::c_ulong::from(time);
        // SAFETY: Xlib consumes one native unsigned long for a format-32 property.
        unsafe {
            (x.XChangeProperty)(
                display,
                window,
                atom(x, display, "_NET_WM_USER_TIME"),
                xlib::XA_CARDINAL,
                32,
                xlib::PropModeReplace,
                (&value as *const std::os::raw::c_ulong).cast(),
                1,
            );
        }
    }
    time
}

/// The EWMH states SDL keeps for a window, as `_NET_WM_STATE` atom names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct States {
    pub above: bool,
    pub utility: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub modal: bool,
    pub sticky: bool,
}

impl States {
    fn names(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.above {
            names.push("_NET_WM_STATE_ABOVE");
        }
        if self.utility {
            names.extend(["_NET_WM_STATE_SKIP_TASKBAR", "_NET_WM_STATE_SKIP_PAGER"]);
        }
        if self.maximized {
            names.extend([
                "_NET_WM_STATE_MAXIMIZED_VERT",
                "_NET_WM_STATE_MAXIMIZED_HORZ",
            ]);
        }
        if self.fullscreen {
            names.push("_NET_WM_STATE_FULLSCREEN");
        }
        if self.modal {
            names.push("_NET_WM_STATE_MODAL");
        }
        if self.sticky {
            names.push("_NET_WM_STATE_STICKY");
        }
        names
    }
}

/// Readies a withdrawn window to map again. The window manager drops a
/// window's `_NET_WM_STATE` when it is withdrawn and decides focus at map time
/// from `_NET_WM_USER_TIME`, so both are written before the map.
pub(crate) fn prepare_map(
    display: *mut std::ffi::c_void,
    window: u64,
    states: States,
    siblings: &[u64],
) -> crate::Result<()> {
    let x = xlib().ok_or_else(|| crate::Error::new("Xlib is unavailable"))?;
    let display = display as *mut Display;
    let window = window as xlib::Window;
    let atoms: Vec<Atom> = states
        .names()
        .into_iter()
        .map(|n| atom(x, display, n))
        .collect();
    let state = atom(x, display, "_NET_WM_STATE");
    let desktop = atom(x, display, "_NET_WM_DESKTOP");
    // SAFETY: these are this live SDL window's X handles; `atoms` and
    // `every` outlive the calls.
    unsafe {
        if atoms.is_empty() {
            (x.XDeleteProperty)(display, window, state);
        } else {
            (x.XChangeProperty)(
                display,
                window,
                state,
                xlib::XA_ATOM,
                32,
                xlib::PropModeReplace,
                atoms.as_ptr().cast(),
                atoms.len() as i32,
            );
        }
        // Without a desktop of its own the window manager maps the window on
        // the current one.
        if states.sticky {
            let every: [c_long; 1] = [ALL_DESKTOPS];
            (x.XChangeProperty)(
                display,
                window,
                desktop,
                xlib::XA_CARDINAL,
                32,
                xlib::PropModeReplace,
                every.as_ptr().cast(),
                1,
            );
        } else {
            (x.XDeleteProperty)(display, window, desktop);
        }
    }
    claim_user_time(x, display, window, siblings);
    // SAFETY: display is the live SDL connection.
    unsafe { (x.XFlush)(display) };
    Ok(())
}

/// Raises the window and asks the window manager to focus it, as of the
/// latest user time the application can claim across its windows.
pub(crate) fn raise(
    display: *mut std::ffi::c_void,
    window: u64,
    screen: i64,
    siblings: &[u64],
) -> crate::Result<()> {
    let x = xlib().ok_or_else(|| crate::Error::new("Xlib is unavailable"))?;
    let display = display as *mut Display;
    let window = window as xlib::Window;
    let time = claim_user_time(x, display, window, siblings);
    // EWMH's source 2 marks a direct user action, which a window manager
    // does not hold back as focus stealing; an application's own request is
    // source 1, weighed against what the user did since.
    let source = if shortcut_entitles(time) { 2 } else { 1 };
    // SAFETY: these are this live SDL window's X handles.
    unsafe { (x.XRaiseWindow)(display, window) };
    ask(
        x,
        display,
        window,
        screen,
        "_NET_ACTIVE_WINDOW",
        [source, time as c_long, 0, 0, 0],
    );
    Ok(())
}

/// Puts a shown window on every desktop, or back on the current one alone.
pub(crate) fn set_sticky(
    display: *mut std::ffi::c_void,
    window: u64,
    screen: i64,
    on: bool,
) -> crate::Result<()> {
    let x = xlib().ok_or_else(|| crate::Error::new("Xlib is unavailable"))?;
    let display = display as *mut Display;
    let window = window as xlib::Window;
    let sticky = atom(x, display, "_NET_WM_STATE_STICKY") as c_long;
    ask(
        x,
        display,
        window,
        screen,
        "_NET_WM_STATE",
        [c_long::from(on), sticky, 0, 1, 0],
    );
    let desktop = if on {
        ALL_DESKTOPS
    } else {
        current_desktop(x, display, screen)
    };
    ask(
        x,
        display,
        window,
        screen,
        "_NET_WM_DESKTOP",
        [desktop, 1, 0, 0, 0],
    );
    Ok(())
}

// _NET_WM_DESKTOP's value for a window on every desktop.
const ALL_DESKTOPS: c_long = 0xFFFF_FFFF;

// The desktop the user is on, or the first when the window manager says none.
fn current_desktop(x: &Xlib, display: *mut Display, screen: i64) -> c_long {
    let mut kind: Atom = 0;
    let (mut format, mut count, mut after) = (0, 0, 0);
    let mut data: *mut u8 = std::ptr::null_mut();
    // SAFETY: display is live; the outputs are valid storage and the data
    // Xlib answers is freed here.
    unsafe {
        let root = (x.XRootWindow)(display, screen as i32);
        let read = (x.XGetWindowProperty)(
            display,
            root,
            atom(x, display, "_NET_CURRENT_DESKTOP"),
            0,
            1,
            0,
            xlib::XA_CARDINAL,
            &mut kind,
            &mut format,
            &mut count,
            &mut after,
            &mut data,
        );
        let desktop = if read == 0 && format == 32 && count == 1 && !data.is_null() {
            *data.cast::<c_long>()
        } else {
            0
        };
        if !data.is_null() {
            (x.XFree)(data.cast());
        }
        desktop
    }
}

// Sends the window manager an EWMH request about `window`, as a normal
// application (source 1).
fn ask(
    x: &Xlib,
    display: *mut Display,
    window: xlib::Window,
    screen: i64,
    request: &str,
    data: [c_long; 5],
) {
    use x11_dl::xlib::{ClientMessageData, XClientMessageEvent, XEvent};
    let mut message = XClientMessageEvent {
        type_: xlib::ClientMessage,
        serial: 0,
        send_event: 1,
        display,
        window,
        message_type: atom(x, display, request),
        format: 32,
        data: ClientMessageData::new(),
    };
    for (i, value) in data.into_iter().enumerate() {
        message.data.set_long(i, value);
    }
    let mut event = XEvent {
        client_message: message,
    };
    // SAFETY: these are this live SDL window's X handles.
    unsafe {
        (x.XSendEvent)(
            display,
            (x.XRootWindow)(display, screen as i32),
            0,
            xlib::SubstructureNotifyMask | xlib::SubstructureRedirectMask,
            &mut event,
        );
        (x.XFlush)(display);
    }
}

#[cfg(test)]
mod tests {
    use super::{States, later};

    #[test]
    fn a_shortcut_press_entitles_one_direct_activation() {
        use std::sync::atomic::Ordering;
        super::OUTSIDE_INPUT.store(500, Ordering::Relaxed);
        assert!(
            !super::shortcut_entitles(700),
            "later input on a window of its own"
        );
        assert!(super::shortcut_entitles(500));
        assert!(!super::shortcut_entitles(500), "spent");
        super::note_outside_input(900);
        assert!(super::shortcut_entitles(900), "a new press");
    }

    #[test]
    fn the_later_server_time_wins_across_a_wrap() {
        assert_eq!(later(20, 10), 20);
        assert_eq!(later(10, 20), 20);
        assert_eq!(later(5, u32::MAX - 5), 5);
        assert_eq!(later(0, 7), 7);
        assert_eq!(later(7, 0), 7);
    }

    #[test]
    fn each_kept_state_names_its_atoms() {
        assert!(States::default().names().is_empty());
        let all = States {
            above: true,
            utility: true,
            maximized: true,
            fullscreen: true,
            modal: true,
            sticky: true,
        };
        assert_eq!(all.names().len(), 8);
        assert_eq!(
            States {
                above: true,
                ..States::default()
            }
            .names(),
            ["_NET_WM_STATE_ABOVE"]
        );
    }
}
