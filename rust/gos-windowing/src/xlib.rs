//! The X11 client library, loaded on first use from the copy SDL loaded.

use std::ffi::CString;
use std::sync::OnceLock;

use x11_dl::xlib::{Atom, Display, Xlib};

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
