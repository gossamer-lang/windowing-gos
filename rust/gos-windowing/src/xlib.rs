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

/// The id of a 32-bit TrueColor visual, whose fourth byte a compositor reads
/// as alpha; `None` when the server offers none or Xlib is missing. Visual
/// ids hold for every connection to a server, so a connection of its own
/// asks, once.
pub(crate) fn argb_visual() -> Option<u64> {
    static VISUAL: OnceLock<Option<u64>> = OnceLock::new();
    *VISUAL.get_or_init(|| {
        let x = xlib()?;
        // SAFETY: a null name opens the display $DISPLAY names, the server
        // SDL connected to.
        let display = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
        if display.is_null() {
            return None;
        }
        let mut info = std::mem::MaybeUninit::<xlib::XVisualInfo>::zeroed();
        // SAFETY: display is a live connection and info receives the match.
        let found = unsafe {
            (x.XMatchVisualInfo)(
                display,
                (x.XDefaultScreen)(display),
                32,
                xlib::TrueColor,
                info.as_mut_ptr(),
            )
        };
        // SAFETY: XMatchVisualInfo filled info when it found a visual.
        let id = (found != 0).then(|| unsafe { info.assume_init() }.visualid);
        // SAFETY: display was opened above and nothing else uses it.
        unsafe { (x.XCloseDisplay)(display) };
        id
    })
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

/// Asks the window manager to stop keeping `window` maximized along either
/// side; a manager that maximizes a window filling the work area's height
/// or width holds that size against later resizes.
pub(crate) fn release_maximized(display: *mut std::ffi::c_void, window: u64, screen: i64) {
    let Some(x) = xlib() else {
        return;
    };
    let display = display as *mut Display;
    let vertical = atom(x, display, "_NET_WM_STATE_MAXIMIZED_VERT") as c_long;
    let horizontal = atom(x, display, "_NET_WM_STATE_MAXIMIZED_HORZ") as c_long;
    ask(
        x,
        display,
        window as xlib::Window,
        screen,
        "_NET_WM_STATE",
        [0, vertical, horizontal, 1, 0],
    );
}

/// Puts `window`'s frame top-left at (`x`, `y`) and gives its content
/// `width` by `height` in one request, so the window manager shows no
/// halfway state.
pub(crate) fn move_resize(
    display: *mut std::ffi::c_void,
    window: u64,
    (x, y): (i64, i64),
    (width, height): (i64, i64),
) {
    let Some(xl) = xlib() else {
        return;
    };
    let display = display as *mut Display;
    // SAFETY: these are this live SDL window's X handles.
    unsafe {
        (xl.XMoveResizeWindow)(
            display,
            window as xlib::Window,
            x as i32,
            y as i32,
            width.max(1) as u32,
            height.max(1) as u32,
        );
        (xl.XFlush)(display);
    }
}

/// Waits until the X server has reported `window` at `size`, for at most
/// `limit`; true when it has. The report stays queued for SDL to read.
pub(crate) fn wait_configured(
    display: *mut std::ffi::c_void,
    window: u64,
    (width, height): (i64, i64),
    limit: std::time::Duration,
) -> bool {
    struct Wanted {
        window: xlib::Window,
        width: i32,
        height: i32,
        seen: bool,
    }
    // Answers false for every event, so XCheckIfEvent only looks at the queue.
    unsafe extern "C" fn look(
        _display: *mut Display,
        event: *mut xlib::XEvent,
        wanted: *mut std::os::raw::c_char,
    ) -> xlib::Bool {
        // SAFETY: Xlib passes a queued event, and `wanted` is the Wanted the
        // caller below lent for this call.
        unsafe {
            let wanted = &mut *(wanted as *mut Wanted);
            if (*event).get_type() == xlib::ConfigureNotify {
                let c = (*event).configure;
                if c.window == wanted.window && c.width == wanted.width && c.height == wanted.height
                {
                    wanted.seen = true;
                }
            }
        }
        xlib::False
    }
    let Some(x) = xlib() else {
        return false;
    };
    let display = display as *mut Display;
    let mut wanted = Wanted {
        window: window as xlib::Window,
        width: width.max(1) as i32,
        height: height.max(1) as i32,
        seen: false,
    };
    let deadline = std::time::Instant::now() + limit;
    // SAFETY: display is SDL's live connection, used on the thread SDL uses
    // it from; `look` removes nothing from the queue.
    let fd = unsafe { (x.XConnectionNumber)(display) };
    loop {
        let mut event = std::mem::MaybeUninit::<xlib::XEvent>::zeroed();
        // SAFETY: as above; XCheckIfEvent also reads what the server sent.
        unsafe {
            (x.XCheckIfEvent)(
                display,
                event.as_mut_ptr(),
                Some(look),
                (&raw mut wanted).cast(),
            );
        }
        if wanted.seen {
            return true;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the connection's descriptor.
        unsafe { libc::poll(&mut poll, 1, left.as_millis().max(1) as i32) };
    }
}

/// Fixes `window`'s size hints at its present size; the hints it had, to
/// put back with `set_size_hints`, or `None` when it has none to keep.
pub(crate) fn fix_size(display: *mut std::ffi::c_void, window: u64) -> Option<xlib::XSizeHints> {
    let x = xlib()?;
    let display = display as *mut Display;
    let window = window as xlib::Window;
    // SAFETY: XSizeHints is plain C data, for which all zeros is a value.
    let mut kept: xlib::XSizeHints = unsafe { std::mem::zeroed() };
    let mut supplied: c_long = 0;
    let (mut root, mut left, mut top) = (0, 0, 0);
    let (mut width, mut height, mut border, mut depth) = (0, 0, 0, 0);
    // SAFETY: these are this live SDL window's X handles; every output is
    // valid storage of its type.
    let read = unsafe {
        (x.XGetGeometry)(
            display,
            window,
            &mut root,
            &mut left,
            &mut top,
            &mut width,
            &mut height,
            &mut border,
            &mut depth,
        ) != 0
            && (x.XGetWMNormalHints)(display, window, &mut kept, &mut supplied) != 0
    };
    if !read {
        return None;
    }
    let mut fixed = kept;
    fixed.flags |= xlib::PMinSize | xlib::PMaxSize;
    (fixed.min_width, fixed.max_width) = (width as i32, width as i32);
    (fixed.min_height, fixed.max_height) = (height as i32, height as i32);
    set_size_hints(display.cast(), window, fixed);
    Some(kept)
}

/// Gives `window` size hints `hints`.
pub(crate) fn set_size_hints(
    display: *mut std::ffi::c_void,
    window: u64,
    mut hints: xlib::XSizeHints,
) {
    let Some(x) = xlib() else {
        return;
    };
    let display = display as *mut Display;
    // SAFETY: these are this live SDL window's X handles; `hints` outlives
    // the call.
    unsafe {
        (x.XSetWMNormalHints)(display, window as xlib::Window, &mut hints);
        (x.XFlush)(display);
    }
}

/// Stacks `window` above the others without moving the focus.
pub(crate) fn restack_above(display: *mut std::ffi::c_void, window: u64, screen: i64) {
    let Some(x) = xlib() else {
        return;
    };
    // Source 2 marks the pager-like direct request a window manager honours;
    // detail 0 is Above, with no sibling.
    ask(
        x,
        display as *mut Display,
        window as xlib::Window,
        screen,
        "_NET_RESTACK_WINDOW",
        [2, 0, 0, 0, 0],
    );
}

/// Whether `window` is on the desktop the user is on: true for a window on
/// every desktop, and when the window manager keeps no desktops.
pub(crate) fn on_current_desktop(display: *mut std::ffi::c_void, window: u64, screen: i64) -> bool {
    let Some(x) = xlib() else {
        return true;
    };
    let display = display as *mut Display;
    // SAFETY: display is the live SDL connection.
    let root = unsafe { (x.XRootWindow)(display, screen as i32) };
    let Some(own) = cardinal(x, display, window as xlib::Window, "_NET_WM_DESKTOP") else {
        return true;
    };
    let Some(current) = cardinal(x, display, root, "_NET_CURRENT_DESKTOP") else {
        return true;
    };
    // Xlib widens the 32-bit value to a long, sign-extended or not.
    own & ALL_DESKTOPS == ALL_DESKTOPS || own == current
}

// The desktop the user is on, or the first when the window manager says none.
fn current_desktop(x: &Xlib, display: *mut Display, screen: i64) -> c_long {
    // SAFETY: display is live.
    let root = unsafe { (x.XRootWindow)(display, screen as i32) };
    cardinal(x, display, root, "_NET_CURRENT_DESKTOP").unwrap_or(0)
}

// The one CARDINAL property `name` of `window`, when it is set.
fn cardinal(x: &Xlib, display: *mut Display, window: xlib::Window, name: &str) -> Option<c_long> {
    let mut kind: Atom = 0;
    let (mut format, mut count, mut after) = (0, 0, 0);
    let mut data: *mut u8 = std::ptr::null_mut();
    // SAFETY: display is live; the outputs are valid storage and the data
    // Xlib answers is freed here.
    unsafe {
        let read = (x.XGetWindowProperty)(
            display,
            window,
            atom(x, display, name),
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
        let value = (read == 0 && format == 32 && count == 1 && !data.is_null())
            .then(|| *data.cast::<c_long>());
        if !data.is_null() {
            (x.XFree)(data.cast());
        }
        value
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
