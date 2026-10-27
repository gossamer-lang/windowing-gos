//! Dragging data out of a window to other applications (or the program's own
//! windows). `App::start_drag`, called while a pointer button is held, offers
//! the items; the platform drives the drag until the button is released or
//! Escape is pressed, and `Event::DragFinished` reports what the target did.
//!
//! X11 speaks the XDND protocol (version 5) from SDL's X11 event hook,
//! Wayland the core data-device protocol, Windows OLE's `DoDragDrop`, and
//! macOS an AppKit dragging session. Other drivers answer an error, which
//! `has_outbound_drag` reports.

use crate::window::Window;
use crate::{App, Error, Result};

/// One representation of the dragged data: its MIME type and bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DragItem {
    pub mime: String,
    pub data: Vec<u8>,
}

/// What a drop does with the data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DragOperation {
    #[default]
    Copy,
    Move,
    Link,
}

/// A drag the program started, as `Event::DragFinished` names it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct DragId(pub u64);

impl From<DragId> for u64 {
    fn from(id: DragId) -> u64 {
        id.0
    }
}

impl From<u64> for DragId {
    fn from(n: u64) -> DragId {
        DragId(n)
    }
}

/// The operation a finished drag's `code` names: 0 for none.
pub(crate) fn operation_from(code: i32) -> Option<DragOperation> {
    match code {
        1 => Some(DragOperation::Copy),
        2 => Some(DragOperation::Move),
        3 => Some(DragOperation::Link),
        _ => None,
    }
}

/// The code `operation_from` reads back.
pub(crate) fn operation_code(operation: Option<DragOperation>) -> i32 {
    match operation {
        Some(DragOperation::Copy) => 1,
        Some(DragOperation::Move) => 2,
        Some(DragOperation::Link) => 3,
        None => 0,
    }
}

impl App {
    /// Starts dragging `items` out of `window`, offering `operation`. Call
    /// it while a pointer button is held; the drag ends when the button is
    /// released or Escape is pressed, and `Event::DragFinished` follows with
    /// the target's operation, or `None` when no target accepted.
    pub fn start_drag(
        &self,
        window: Window,
        items: Vec<DragItem>,
        operation: DragOperation,
    ) -> Result<DragId> {
        crate::support::require_main_thread("App::start_drag");
        if items.is_empty() {
            return Err(Error::new("a drag needs at least one item"));
        }
        let finished = self.event_base + crate::event::DRAG_OFFSET;
        match crate::video_driver().as_str() {
            #[cfg(target_os = "linux")]
            "x11" => xdnd::start(window, items, operation, finished),
            #[cfg(target_os = "linux")]
            "wayland" => wayland::start(
                window,
                items,
                operation,
                finished,
                self.event_base + crate::event::LOCAL_DROP_OFFSET,
            ),
            "windows" => crate::ole::start_drag(items, operation, finished),
            "cocoa" => crate::macos::start_drag(window, items, operation, finished),
            driver => Err(Error::new(format!(
                "dragging data out needs X11 or Wayland here; the {driver} driver has no drag source yet"
            ))),
        }
    }
}

/// Whether `App::start_drag` can drag data to other applications here.
pub fn has_outbound_drag() -> bool {
    match crate::video_driver().as_str() {
        "x11" => cfg!(target_os = "linux"),
        #[cfg(target_os = "linux")]
        "wayland" => wayland::available(),
        "windows" | "cocoa" => true,
        _ => false,
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn has_activation() -> bool {
    wayland::has_activation()
}

/// Binds what a drag source needs once the video driver is up.
pub(crate) fn init() {
    #[cfg(target_os = "linux")]
    if crate::video_driver() == "wayland" {
        wayland::init();
    }
}

/// The text the next local drop event carries.
pub(crate) fn take_local_text() -> String {
    #[cfg(target_os = "linux")]
    return wayland::take_local_text();
    #[cfg(not(target_os = "linux"))]
    String::new()
}

/// Releases a finished drag as its `Event::DragFinished` is decoded.
pub(crate) fn finished() {
    #[cfg(target_os = "linux")]
    wayland::reap();
}

/// Ends the drag sources when the session ends.
pub(crate) fn shutdown() {
    #[cfg(target_os = "linux")]
    {
        xdnd::shutdown();
        wayland::shutdown();
    }
}

#[cfg(target_os = "linux")]
mod xdnd {
    use std::ffi::{CString, c_int, c_long, c_void};
    use std::sync::OnceLock;

    use parking_lot::Mutex;
    use sdl3_sys::properties::{SDL_GetNumberProperty, SDL_GetPointerProperty};
    use sdl3_sys::system::{SDL_SetX11EventHook, XEvent as SdlXEvent};
    use sdl3_sys::video::{
        SDL_GetWindowProperties, SDL_PROP_WINDOW_X11_DISPLAY_POINTER,
        SDL_PROP_WINDOW_X11_WINDOW_NUMBER,
    };
    use x11_dl::xinput2::XIDeviceEvent;
    use x11_dl::xlib::{self, Atom, Display, XEvent, Xlib};

    use super::{DragId, DragItem, DragOperation, operation_code};
    use crate::window::Window;
    use crate::{Error, Result};

    const XI_KEY_PRESS: c_int = 2;
    const XI_BUTTON_RELEASE: c_int = 5;
    const XI_MOTION: c_int = 6;
    const ESCAPE_KEYSYM: u64 = 0xFF1B;
    const XDND_VERSION: i64 = 5;

    // The drag in progress. Only the main thread touches it: SDL runs the
    // X11 hook inside its event pump.
    struct Drag {
        id: u64,
        display: usize,
        source: u64,
        items: Vec<DragItem>,
        types: Vec<Atom>,
        action: Atom,
        target: u64,
        target_version: i64,
        accepted: bool,
        accepted_action: Atom,
        dropped: bool,
        event_type: u32,
        // The XInput2 major opcode, whose generic events carry the pointer
        // and keyboard when SDL reads them through XInput2; 0 without it.
        xinput: c_int,
    }

    static ACTIVE: Mutex<Option<Drag>> = Mutex::new(None);
    static NEXT_ID: Mutex<u64> = Mutex::new(1);
    static HOOKED: Mutex<bool> = Mutex::new(false);

    fn xlib() -> Option<&'static Xlib> {
        static XLIB: OnceLock<Option<Xlib>> = OnceLock::new();
        XLIB.get_or_init(|| Xlib::open().ok()).as_ref()
    }

    fn atom(x: &Xlib, display: *mut Display, name: &str) -> Atom {
        let Ok(text) = CString::new(name) else {
            return 0;
        };
        // SAFETY: display is SDL's live connection; text outlives the call.
        unsafe { (x.XInternAtom)(display, text.as_ptr(), 0) }
    }

    fn action_atom(x: &Xlib, display: *mut Display, operation: DragOperation) -> Atom {
        match operation {
            DragOperation::Copy => atom(x, display, "XdndActionCopy"),
            DragOperation::Move => atom(x, display, "XdndActionMove"),
            DragOperation::Link => atom(x, display, "XdndActionLink"),
        }
    }

    fn operation_of(x: &Xlib, display: *mut Display, action: Atom) -> Option<DragOperation> {
        if action == 0 {
            None
        } else if action == atom(x, display, "XdndActionMove") {
            Some(DragOperation::Move)
        } else if action == atom(x, display, "XdndActionLink") {
            Some(DragOperation::Link)
        } else {
            Some(DragOperation::Copy)
        }
    }

    pub(super) fn start(
        window: Window,
        items: Vec<DragItem>,
        operation: DragOperation,
        event_type: u32,
    ) -> Result<DragId> {
        let mut active = ACTIVE.lock();
        if active.is_some() {
            return Err(Error::new("a drag is already in progress"));
        }
        let x = xlib().ok_or_else(|| Error::new("loading the X11 library"))?;
        // SAFETY: the window is live; SDL answers null or 0 when unset.
        let (display, source) = unsafe {
            let props = SDL_GetWindowProperties(window.handle);
            (
                SDL_GetPointerProperty(
                    props,
                    SDL_PROP_WINDOW_X11_DISPLAY_POINTER,
                    std::ptr::null_mut(),
                ),
                SDL_GetNumberProperty(props, SDL_PROP_WINDOW_X11_WINDOW_NUMBER, 0) as u64,
            )
        };
        if display.is_null() {
            return Err(Error::new("the window has no X11 display"));
        }
        if source == 0 {
            return Err(Error::new("the window has no X11 window"));
        }
        let display = display.cast::<Display>();
        let types: Vec<Atom> = items
            .iter()
            .map(|item| atom(x, display, &item.mime))
            .collect();
        let longs: Vec<c_long> = types.iter().map(|a| *a as c_long).collect();
        // SAFETY: display and source are live; the data outlives the call.
        unsafe {
            (x.XChangeProperty)(
                display,
                source,
                atom(x, display, "XdndTypeList"),
                xlib::XA_ATOM,
                32,
                xlib::PropModeReplace,
                longs.as_ptr().cast(),
                longs.len() as c_int,
            );
            (x.XSetSelectionOwner)(display, atom(x, display, "XdndSelection"), source, 0);
        }
        let mut hooked = HOOKED.lock();
        if !*hooked {
            // SAFETY: the hook stays valid for the program's life.
            unsafe { SDL_SetX11EventHook(Some(on_event), std::ptr::null_mut()) };
            *hooked = true;
        }
        let mut next = NEXT_ID.lock();
        let id = *next;
        *next += 1;
        *active = Some(Drag {
            id,
            display: display as usize,
            source,
            items,
            types,
            action: action_atom(x, display, operation),
            target: 0,
            target_version: 0,
            accepted: false,
            accepted_action: 0,
            dropped: false,
            event_type,
            xinput: xinput_opcode(x, display),
        });
        Ok(DragId(id))
    }

    fn xinput_opcode(x: &Xlib, display: *mut Display) -> c_int {
        let name = CString::new("XInputExtension").expect("a literal without NUL");
        let (mut opcode, mut first_event, mut first_error) = (0, 0, 0);
        // SAFETY: display is live; the outputs are valid storage.
        if unsafe {
            (x.XQueryExtension)(
                display,
                name.as_ptr(),
                &mut opcode,
                &mut first_event,
                &mut first_error,
            )
        } == 0
        {
            return 0;
        }
        opcode
    }

    pub(super) fn shutdown() {
        let mut hooked = HOOKED.lock();
        if *hooked {
            // SAFETY: clearing the hook SDL calls.
            unsafe { SDL_SetX11EventHook(None, std::ptr::null_mut()) };
            *hooked = false;
        }
        *ACTIVE.lock() = None;
    }

    fn client_message(
        x: &Xlib,
        display: *mut Display,
        to: u64,
        kind: &str,
        data: [c_long; 5],
    ) -> XEvent {
        let mut message = xlib::XClientMessageEvent {
            type_: xlib::ClientMessage,
            serial: 0,
            send_event: 0,
            display,
            window: to,
            message_type: atom(x, display, kind),
            format: 32,
            data: xlib::ClientMessageData::new(),
        };
        for (i, word) in data.iter().enumerate() {
            message.data.set_long(i, *word);
        }
        XEvent {
            client_message: message,
        }
    }

    fn send(x: &Xlib, display: *mut Display, to: u64, mut event: XEvent) {
        // SAFETY: display is live; event is a complete X event.
        unsafe {
            (x.XSendEvent)(display, to, 0, 0, &mut event);
            (x.XFlush)(display);
        }
    }

    // The XDND version `window` advertises, or 0 when it takes no drops.
    fn aware_version(x: &Xlib, display: *mut Display, window: u64) -> i64 {
        let (mut kind, mut bits, mut count, mut after) = (0, 0, 0, 0);
        let mut data: *mut u8 = std::ptr::null_mut();
        // SAFETY: display is live; the outputs are valid storage.
        let status = unsafe {
            (x.XGetWindowProperty)(
                display,
                window,
                atom(x, display, "XdndAware"),
                0,
                1,
                0,
                xlib::XA_ATOM,
                &mut kind,
                &mut bits,
                &mut count,
                &mut after,
                &mut data,
            )
        };
        if data.is_null() {
            return 0;
        }
        // SAFETY: a format-32 property holds longs; one was asked for.
        let version = if status != 0 || count == 0 || bits != 32 {
            0
        } else {
            (unsafe { *data.cast::<c_long>() }) as i64
        };
        // SAFETY: Xlib allocated data for the caller to free.
        unsafe { (x.XFree)(data.cast()) };
        version
    }

    // The deepest window under the root point (`rx`, `ry`) that takes drops.
    fn target_at(x: &Xlib, display: *mut Display, rx: i32, ry: i32) -> (u64, i64) {
        // SAFETY: display is live.
        let root = unsafe { (x.XDefaultRootWindow)(display) };
        let mut current = root;
        loop {
            let version = aware_version(x, display, current);
            if version > 0 {
                return (current, version);
            }
            let (mut tx, mut ty, mut child) = (0, 0, 0);
            // SAFETY: display is live; the outputs are valid storage.
            let ok = unsafe {
                (x.XTranslateCoordinates)(
                    display, root, current, rx, ry, &mut tx, &mut ty, &mut child,
                )
            };
            if ok == 0 || child == 0 {
                return (0, 0);
            }
            current = child;
        }
    }

    fn enter(x: &Xlib, drag: &Drag, display: *mut Display) {
        let more = c_long::from(drag.types.len() > 3);
        let version = drag.target_version.min(XDND_VERSION) as c_long;
        let mut data = [drag.source as c_long, (version << 24) | more, 0, 0, 0];
        for (i, t) in drag.types.iter().take(3).enumerate() {
            data[2 + i] = *t as c_long;
        }
        send(
            x,
            display,
            drag.target,
            client_message(x, display, drag.target, "XdndEnter", data),
        );
    }

    fn leave(x: &Xlib, drag: &Drag, display: *mut Display) {
        let data = [drag.source as c_long, 0, 0, 0, 0];
        send(
            x,
            display,
            drag.target,
            client_message(x, display, drag.target, "XdndLeave", data),
        );
    }

    fn finish(active: &mut Option<Drag>, operation: Option<DragOperation>) {
        let Some(drag) = active.take() else { return };
        // The hook answers nothing; a refused event is a lost report.
        let _ = crate::waker::push_user_event(
            drag.event_type,
            0,
            operation_code(operation),
            drag.id,
            0,
        );
    }

    fn on_motion(x: &Xlib, active: &mut Option<Drag>, rx: i32, ry: i32, time: u64) {
        let Some(drag) = active.as_mut() else { return };
        // The pointer and keyboard may each arrive as both a core and an
        // XInput2 event; after the drop only the target's reply matters.
        if drag.dropped {
            return;
        }
        let display = drag.display as *mut Display;
        let (target, version) = target_at(x, display, rx, ry);
        if target != drag.target {
            if drag.target != 0 {
                leave(x, drag, display);
            }
            drag.target = target;
            drag.target_version = version;
            drag.accepted = false;
            drag.accepted_action = 0;
            if target != 0 {
                enter(x, drag, display);
            }
        }
        if drag.target != 0 {
            let packed = ((rx as c_long) << 16) | (ry as c_long & 0xFFFF);
            let data = [
                drag.source as c_long,
                0,
                packed,
                time as c_long,
                drag.action as c_long,
            ];
            send(
                x,
                display,
                drag.target,
                client_message(x, display, drag.target, "XdndPosition", data),
            );
        }
    }

    fn on_release(x: &Xlib, active: &mut Option<Drag>, time: u64) {
        let Some(drag) = active.as_mut() else { return };
        if drag.dropped {
            return;
        }
        let display = drag.display as *mut Display;
        if drag.target != 0 && drag.accepted {
            drag.dropped = true;
            let data = [drag.source as c_long, 0, time as c_long, 0, 0];
            send(
                x,
                display,
                drag.target,
                client_message(x, display, drag.target, "XdndDrop", data),
            );
            return;
        }
        if drag.target != 0 {
            leave(x, drag, display);
        }
        finish(active, None);
    }

    fn on_key(x: &Xlib, active: &mut Option<Drag>, keycode: u32) {
        let Some(drag) = active.as_ref() else { return };
        let display = drag.display as *mut Display;
        // SAFETY: display is live.
        let escape = unsafe { (x.XKeysymToKeycode)(display, ESCAPE_KEYSYM) };
        if keycode != u32::from(escape) || drag.dropped {
            return;
        }
        if drag.target != 0 {
            leave(x, drag, display);
        }
        finish(active, None);
    }

    // Answers true for the messages this drag consumes.
    fn on_client_message(
        x: &Xlib,
        active: &mut Option<Drag>,
        event: &xlib::XClientMessageEvent,
    ) -> bool {
        let Some(drag) = active.as_mut() else {
            return false;
        };
        let display = drag.display as *mut Display;
        let from = event.data.get_long(0) as u64;
        if event.message_type == atom(x, display, "XdndStatus") && from == drag.target {
            drag.accepted = event.data.get_long(1) & 1 != 0;
            drag.accepted_action = event.data.get_long(4) as Atom;
            return true;
        }
        if event.message_type == atom(x, display, "XdndFinished") && from == drag.target {
            let accepted = drag.target_version < 5 || event.data.get_long(1) & 1 != 0;
            let action = if drag.target_version >= 5 {
                event.data.get_long(2) as Atom
            } else {
                drag.accepted_action
            };
            let operation = if accepted {
                operation_of(x, display, action)
            } else {
                None
            };
            finish(active, operation);
            return true;
        }
        false
    }

    // Answers true when the request was for this drag's selection.
    fn on_selection_request(
        x: &Xlib,
        active: &Option<Drag>,
        request: &xlib::XSelectionRequestEvent,
    ) -> bool {
        let Some(drag) = active.as_ref() else {
            return false;
        };
        let display = drag.display as *mut Display;
        if request.selection != atom(x, display, "XdndSelection") {
            return false;
        }
        let mut property = request.property;
        let served = drag
            .items
            .iter()
            .find(|item| atom(x, display, &item.mime) == request.target);
        match served {
            // SAFETY: display and the requestor are live; data outlives the call.
            Some(item) => unsafe {
                (x.XChangeProperty)(
                    display,
                    request.requestor,
                    property,
                    request.target,
                    8,
                    xlib::PropModeReplace,
                    item.data.as_ptr(),
                    item.data.len() as c_int,
                );
            },
            None => property = 0,
        }
        let notify = xlib::XSelectionEvent {
            type_: xlib::SelectionNotify,
            serial: 0,
            send_event: 0,
            display,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property,
            time: request.time,
        };
        send(x, display, request.requestor, XEvent { selection: notify });
        true
    }

    // SDL runs this for every X event before handling it, and handles the
    // event only when it answers true; the drag's own messages answer false.
    unsafe extern "C" fn on_event(_userdata: *mut c_void, raw: *mut SdlXEvent) -> bool {
        let Some(x) = xlib() else { return true };
        let mut active = ACTIVE.lock();
        let Some(xinput) = active.as_ref().map(|d| d.xinput) else {
            return true;
        };
        // SAFETY: SDL hands a complete Xlib event.
        let event = unsafe { &*raw.cast::<XEvent>() };
        // SAFETY: each arm reads the union member the event type names.
        unsafe {
            match event.get_type() {
                xlib::MotionNotify => on_motion(
                    x,
                    &mut active,
                    event.motion.x_root,
                    event.motion.y_root,
                    event.motion.time,
                ),
                xlib::ButtonRelease => on_release(x, &mut active, event.button.time),
                xlib::KeyPress => on_key(x, &mut active, event.key.keycode),
                xlib::GenericEvent => {
                    let cookie = &event.generic_event_cookie;
                    if xinput != 0 && cookie.extension == xinput && !cookie.data.is_null() {
                        let device = &*cookie.data.cast::<XIDeviceEvent>();
                        match cookie.evtype {
                            XI_MOTION => on_motion(
                                x,
                                &mut active,
                                device.root_x as i32,
                                device.root_y as i32,
                                device.time,
                            ),
                            XI_BUTTON_RELEASE => on_release(x, &mut active, device.time),
                            XI_KEY_PRESS => on_key(x, &mut active, device.detail as u32),
                            _ => {}
                        }
                    }
                }
                xlib::ClientMessage => {
                    return !on_client_message(x, &mut active, &event.client_message);
                }
                xlib::SelectionRequest => {
                    return !on_selection_request(x, &active, &event.selection_request);
                }
                _ => {}
            }
        }
        true
    }
}

/// Applies a token forwarded by the desktop to SDL's existing Wayland surface.
#[cfg(target_os = "linux")]
pub(crate) fn activate(window: &Window, token: &str) -> Result<()> {
    wayland::activate(window, token)
}
#[cfg(target_os = "linux")]
pub(crate) fn activation_token(window: &Window) -> Result<String> {
    wayland::activation_token(window)
}

// Wayland speaks the core data-device protocol on SDL's own connection. A
// seat and pointer of the library's own record the serial of the press a drag
// starts from. A drag creates a data device of its own for its lifetime:
// compositors route a drag's offers to the client's newest data device, so the
// library delivers a drop on one of its own windows from the drag's items, as
// SDL would have reported it. The library's objects live on an event queue a
// thread of its own dispatches, so a target in another process gets the data
// even while the main thread is busy.
#[cfg(target_os = "linux")]
mod wayland {
    use std::collections::VecDeque;
    use std::io::Write;
    use std::os::fd::{AsFd, AsRawFd, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use parking_lot::{Condvar, Mutex};
    use sdl3_sys::properties::{SDL_GetGlobalProperties, SDL_GetPointerProperty};
    use sdl3_sys::video::{
        SDL_GetWindowID, SDL_GetWindowProperties, SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER,
    };
    use wayland_backend::client::{Backend, WaylandError};
    use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
    use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
    use wayland_client::protocol::wl_data_offer::WlDataOffer;
    use wayland_client::protocol::wl_data_source::{self, WlDataSource};
    use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
    use wayland_client::protocol::wl_pointer::{self, WlPointer};
    use wayland_client::protocol::wl_registry::{self, WlRegistry};
    use wayland_client::protocol::wl_seat::{self, WlSeat};
    use wayland_client::{
        Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop,
    };

    use super::{DragId, DragItem, DragOperation, operation_code};
    use crate::window::Window;
    use crate::{Error, Result};
    use wayland_protocols::xdg::activation::v1::client::{
        xdg_activation_token_v1::{self, XdgActivationTokenV1},
        xdg_activation_v1::XdgActivationV1,
    };

    #[derive(Default)]
    struct PendingToken {
        value: Mutex<Option<String>>,
        ready: Condvar,
    }
    impl Dispatch<XdgActivationTokenV1, Arc<PendingToken>> for Shared {
        fn event(
            _: &mut Self,
            _: &XdgActivationTokenV1,
            event: xdg_activation_token_v1::Event,
            pending: &Arc<PendingToken>,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let xdg_activation_token_v1::Event::Done { token } = event {
                *pending.value.lock() = Some(token);
                pending.ready.notify_all();
            }
        }
    }

    /// What the dispatch thread and the main thread share.
    #[derive(Default)]
    struct State {
        seat: Option<WlSeat>,
        pointer: Option<WlPointer>,
        keyboard: Option<WlKeyboard>,
        activation_serial: u32,
        seat_version: u32,
        manager: Option<WlDataDeviceManager>,
        manager_version: u32,
        seat_name: u32,
        manager_name: u32,
        activation_name: u32,
        activation: Option<XdgActivationV1>,
        focus: Option<u64>,
        press_serial: u32,
        press_surface: Option<u64>,
        held: i64,
        drag: Option<Drag>,
    }

    struct Drag {
        id: u64,
        items: Vec<DragItem>,
        source: WlDataSource,
        device: Option<WlDataDevice>,
        offer: Option<WlDataOffer>,
        target_window: u32,
        accepted: String,
        x: f64,
        y: f64,
        event_type: u32,
        local_type: u32,
        action: Option<DndAction>,
        done: bool,
    }

    struct Session {
        state: Arc<Mutex<State>>,
        stop: Arc<AtomicBool>,
        wake: OwnedFd,
        thread: Option<JoinHandle<()>>,
        handle: QueueHandle<Shared>,
        connection: Connection,
    }

    /// The dispatch target: the shared state behind its lock.
    struct Shared(Arc<Mutex<State>>);

    static SESSION: Mutex<Option<Session>> = Mutex::new(None);
    static NEXT_ID: Mutex<u64> = Mutex::new(1);
    static LOCAL_TEXTS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

    fn key(p: &impl Proxy) -> u64 {
        p.id().as_ptr() as u64
    }

    impl Dispatch<WlRegistry, ()> for Shared {
        fn event(
            s: &mut Self,
            _: &WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            {
                let mut st = s.0.lock();
                if interface == "wl_seat" && st.seat_name == 0 {
                    st.seat_name = name;
                    st.seat_version = version.min(7);
                } else if interface == "wl_data_device_manager" {
                    st.manager_name = name;
                    st.manager_version = version.min(3);
                } else if interface == "xdg_activation_v1" {
                    st.activation_name = name;
                }
            }
        }
    }

    impl Dispatch<WlSeat, ()> for Shared {
        fn event(
            s: &mut Self,
            seat: &WlSeat,
            event: wl_seat::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(caps),
            } = event
            {
                let mut st = s.0.lock();
                if caps.contains(wl_seat::Capability::Pointer) && st.pointer.is_none() {
                    st.pointer = Some(seat.get_pointer(qh, ()));
                }
                if caps.contains(wl_seat::Capability::Keyboard) && st.keyboard.is_none() {
                    st.keyboard = Some(seat.get_keyboard(qh, ()));
                }
            }
        }
    }

    impl Dispatch<WlKeyboard, ()> for Shared {
        fn event(
            state: &mut Self,
            _: &WlKeyboard,
            event: wl_keyboard::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_keyboard::Event::Enter { serial, .. }
            | wl_keyboard::Event::Key { serial, .. } = event
            {
                state.0.lock().activation_serial = serial;
            }
        }
    }

    impl Dispatch<WlPointer, ()> for Shared {
        fn event(
            s: &mut Self,
            _: &WlPointer,
            event: wl_pointer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let mut st = s.0.lock();
            match event {
                wl_pointer::Event::Enter { surface, .. } => st.focus = Some(key(&surface)),
                wl_pointer::Event::Leave { .. } => {
                    st.focus = None;
                    st.held = 0;
                }
                wl_pointer::Event::Button {
                    serial,
                    state: WEnum::Value(state),
                    ..
                } => {
                    if state == wl_pointer::ButtonState::Pressed {
                        st.held += 1;
                        st.press_serial = serial;
                        st.activation_serial = serial;
                        st.press_surface = st.focus;
                    } else if st.held > 0 {
                        st.held -= 1;
                    }
                }
                _ => {}
            }
        }
    }

    delegate_noop!(Shared: ignore WlDataDeviceManager);
    delegate_noop!(Shared: ignore XdgActivationV1);

    impl Dispatch<WlDataSource, ()> for Shared {
        fn event(
            s: &mut Self,
            _: &WlDataSource,
            event: wl_data_source::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let mut st = s.0.lock();
            let Some(drag) = st.drag.as_mut() else { return };
            match event {
                wl_data_source::Event::Send { mime_type, fd } => {
                    send_data(&drag.items, &mime_type, fd)
                }
                wl_data_source::Event::Cancelled => end(drag, None),
                wl_data_source::Event::DndFinished => {
                    let operation = match drag.action {
                        Some(a) if a.contains(DndAction::Move) => Some(DragOperation::Move),
                        Some(a) if a.contains(DndAction::Copy) => Some(DragOperation::Copy),
                        _ => None,
                    };
                    end(drag, operation);
                }
                wl_data_source::Event::Action {
                    dnd_action: WEnum::Value(a),
                } => drag.action = Some(a),
                _ => {}
            }
        }
    }

    impl Dispatch<WlDataOffer, ()> for Shared {
        fn event(
            _: &mut Self,
            _: &WlDataOffer,
            _: wayland_client::protocol::wl_data_offer::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WlDataDevice, ()> for Shared {
        fn event(
            s: &mut Self,
            _: &WlDataDevice,
            event: wl_data_device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            let mut st = s.0.lock();
            let Some(drag) = st.drag.as_mut() else {
                if let wl_data_device::Event::Enter {
                    id: Some(offer), ..
                }
                | wl_data_device::Event::Selection { id: Some(offer) } = event
                {
                    offer.destroy();
                }
                return;
            };
            match event {
                wl_data_device::Event::Enter {
                    serial,
                    surface,
                    x,
                    y,
                    id,
                } => {
                    release_offer(drag);
                    drag.target_window = window_of(key(&surface));
                    drag.x = x;
                    drag.y = y;
                    drag.accepted.clear();
                    if let Some(offer) = &id
                        && drag.target_window != 0
                    {
                        if let Some(item) = deliverable(&drag.items) {
                            offer.accept(serial, Some(item.mime.clone()));
                            if offer.version() >= 3 {
                                offer.set_actions(
                                    DndAction::Copy | DndAction::Move,
                                    DndAction::Copy,
                                );
                            }
                            drag.accepted = item.mime.clone();
                        }
                        local(drag, 1, None);
                    }
                    drag.offer = id;
                }
                wl_data_device::Event::Leave => {
                    release_offer(drag);
                    drag.target_window = 0;
                }
                wl_data_device::Event::Motion { x, y, .. } => {
                    drag.x = x;
                    drag.y = y;
                    if drag.target_window != 0 {
                        local(drag, 1, None);
                    }
                }
                wl_data_device::Event::Drop => {
                    if drag.target_window != 0 && !drag.accepted.is_empty() {
                        if let Some(item) =
                            drag.items.iter().find(|i| i.mime == drag.accepted).cloned()
                        {
                            let text = String::from_utf8(item.data).unwrap_or_default();
                            local(drag, 0, None);
                            for line in text.split('\n') {
                                let line = line.trim_end_matches('\r');
                                if line.is_empty() {
                                    continue;
                                }
                                if item.mime == "text/uri-list" {
                                    if let Some(path) = crate::instance::uri_path(line) {
                                        local(drag, 3, Some(path));
                                    }
                                } else {
                                    local(drag, 2, Some(line.to_string()));
                                }
                            }
                            local(drag, 4, None);
                        }
                        if let Some(offer) = &drag.offer
                            && offer.version() >= 3
                        {
                            offer.finish();
                        }
                    }
                    release_offer(drag);
                    drag.target_window = 0;
                }
                wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
                _ => {}
            }
        }
        wayland_client::event_created_child!(Shared, WlDataDevice, [
            wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
        ]);
    }

    fn send_data(items: &[DragItem], mime: &str, fd: OwnedFd) {
        let Some(item) = items.iter().find(|i| i.mime == mime) else {
            return;
        };
        let mut file = std::fs::File::from(fd);
        // A target that closes early ends the transfer; nothing to report.
        let _ = file.write_all(&item.data);
    }

    fn end(drag: &mut Drag, operation: Option<DragOperation>) {
        if drag.done {
            return;
        }
        drag.done = true;
        let _ = crate::waker::push_user_event(
            drag.event_type,
            0,
            operation_code(operation),
            drag.id,
            0,
        );
    }

    fn release_offer(drag: &mut Drag) {
        if let Some(offer) = drag.offer.take() {
            offer.destroy();
        }
    }

    fn deliverable(items: &[DragItem]) -> Option<&DragItem> {
        ["text/uri-list", "text/plain;charset=utf-8", "text/plain"]
            .iter()
            .find_map(|mime| items.iter().find(|item| item.mime == *mime))
    }

    fn local(drag: &Drag, kind: i32, text: Option<String>) {
        if let Some(text) = text {
            LOCAL_TEXTS.lock().push_back(text);
        }
        let _ = crate::waker::push_user_event(
            drag.local_type,
            drag.target_window,
            kind,
            drag.x.to_bits(),
            drag.y.to_bits(),
        );
    }

    pub(super) fn take_local_text() -> String {
        LOCAL_TEXTS.lock().pop_front().unwrap_or_default()
    }

    fn surface_of(window: &Window) -> Option<u64> {
        let p = window.pointer_property(SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER);
        (!p.is_null()).then_some(p as u64)
    }

    fn window_of(surface: u64) -> u32 {
        crate::window::all_windows()
            .into_iter()
            .find(|w| {
                // SAFETY: w is an open window; SDL answers null when unset.
                let p = unsafe {
                    SDL_GetPointerProperty(
                        SDL_GetWindowProperties(*w),
                        SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER,
                        std::ptr::null_mut(),
                    )
                };
                p as u64 == surface
            })
            // SAFETY: w is an open window.
            .map_or(0, |w| unsafe { SDL_GetWindowID(w) }.0)
    }

    pub(super) fn init() {
        // SAFETY: SDL's global property set is live while video runs.
        let display = unsafe {
            SDL_GetPointerProperty(
                SDL_GetGlobalProperties(),
                c"SDL.video.wayland.wl_display".as_ptr(),
                std::ptr::null_mut(),
            )
        };
        if display.is_null() {
            return;
        }
        // SAFETY: SDL keeps its wl_display open until video quits, after `shutdown`.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        let mut queue: EventQueue<Shared> = connection.new_event_queue();
        let qh = queue.handle();
        let state = Arc::new(Mutex::new(State::default()));
        let mut shared = Shared(state.clone());
        let registry = connection.display().get_registry(&qh, ());
        if queue.roundtrip(&mut shared).is_err() {
            return;
        }
        let (seat_name, seat_version, manager_name, manager_version, activation_name) = {
            let st = state.lock();
            (
                st.seat_name,
                st.seat_version,
                st.manager_name,
                st.manager_version,
                st.activation_name,
            )
        };
        if seat_name == 0 && activation_name == 0 {
            return;
        }
        {
            let mut st = state.lock();
            if seat_name != 0 {
                st.seat = Some(registry.bind(seat_name, seat_version, &qh, ()));
            }
            if manager_name != 0 {
                st.manager = Some(registry.bind(manager_name, manager_version, &qh, ()));
            }
            if activation_name != 0 {
                st.activation = Some(registry.bind(activation_name, 1, &qh, ()));
            }
        }
        if queue.roundtrip(&mut shared).is_err() {
            return;
        }
        let Ok(wake) = rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC) else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let wake = wake.try_clone();
            let Ok(wake) = wake else { return };
            std::thread::Builder::new()
                .name("windowing-wayland-drag".into())
                .spawn(move || pump(queue, shared, &stop, &wake))
                .ok()
        };
        *SESSION.lock() = Some(Session {
            state,
            stop,
            wake,
            thread,
            handle: qh,
            connection,
        });
    }

    /// Whether a connection error ends the connection. A non-blocking flush
    /// or read that would block is not one; anything else means the
    /// compositor is gone or broke the protocol.
    fn fatal(error: &WaylandError) -> bool {
        !matches!(error, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
    }

    // Dispatches the library's queue until `stop` is set or the connection
    // ends: reads the socket when the compositor writes, wakes when
    // `shutdown` writes `wake`.
    fn pump(mut queue: EventQueue<Shared>, mut shared: Shared, stop: &AtomicBool, wake: &OwnedFd) {
        use rustix::event::{PollFd, PollFlags, poll};
        while !stop.load(Ordering::Acquire) {
            if queue.dispatch_pending(&mut shared).is_err() {
                return;
            }
            if let Err(error) = queue.flush()
                && fatal(&error)
            {
                return;
            }
            let Some(guard) = queue.prepare_read() else {
                continue;
            };
            let socket = guard.connection_fd();
            let wake_fd = wake.as_fd();
            let mut fds = [
                PollFd::new(&socket, PollFlags::IN),
                PollFd::new(&wake_fd, PollFlags::IN),
            ];
            let polled = poll(&mut fds, None).is_ok();
            if polled && fds[1].revents().contains(PollFlags::IN) {
                let mut value = [0u8; 8];
                let _ = rustix::io::read(wake, &mut value);
            }
            if polled
                && fds[0]
                    .revents()
                    .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
            {
                if let Err(error) = guard.read()
                    && fatal(&error)
                {
                    return;
                }
            } else {
                drop(guard);
            }
        }
    }

    /// Whether a drag can start here: a seat with a pointer and data devices.
    pub(super) fn available() -> bool {
        SESSION
            .lock()
            .as_ref()
            .is_some_and(|s| s.state.lock().pointer.is_some())
    }

    pub(super) fn has_activation() -> bool {
        SESSION
            .lock()
            .as_ref()
            .is_some_and(|s| s.state.lock().activation.is_some())
    }

    fn actions_of(operation: DragOperation) -> Result<DndAction> {
        match operation {
            DragOperation::Copy => Ok(DndAction::Copy),
            DragOperation::Move => Ok(DndAction::Move),
            DragOperation::Link => Err(Error::new(
                "Wayland drags copy or move; a link has no Wayland action",
            )),
        }
    }

    pub(super) fn start(
        window: Window,
        items: Vec<DragItem>,
        operation: DragOperation,
        event_type: u32,
        local_type: u32,
    ) -> Result<DragId> {
        reap();
        let session = SESSION.lock();
        let Some(session) = session.as_ref() else {
            return Err(Error::new(
                "this Wayland compositor offers no data devices to drag with",
            ));
        };
        let mut st = session.state.lock();
        if st.drag.is_some() {
            return Err(Error::new("a drag is already in progress"));
        }
        let actions = actions_of(operation)?;
        let surface = surface_of(&window);
        if st.held == 0 || st.press_surface.is_none() || st.press_surface != surface {
            return Err(Error::new(
                "start a drag while a pointer button pressed in its window is held",
            ));
        }
        let (Some(manager), Some(seat)) = (st.manager.clone(), st.seat.clone()) else {
            return Err(Error::new("the Wayland data device manager is gone"));
        };
        let qh = &session.handle;
        let source = manager.create_data_source(qh, ());
        for item in &items {
            source.offer(item.mime.clone());
        }
        if st.manager_version >= 3 {
            source.set_actions(actions);
        }
        let device = manager.get_data_device(&seat, qh, ());
        let mut next = NEXT_ID.lock();
        let id = *next;
        *next += 1;
        // The window's surface is SDL's proxy; the request names it by id.
        let origin = window_surface_proxy(&session.connection, surface.unwrap_or(0));
        device.start_drag(
            Some(&source),
            origin
                .as_ref()
                .ok_or_else(|| Error::new("the window has no Wayland surface"))?,
            None,
            st.press_serial,
        );
        st.drag = Some(Drag {
            id,
            items,
            source,
            device: Some(device),
            offer: None,
            target_window: 0,
            accepted: String::new(),
            x: 0.0,
            y: 0.0,
            event_type,
            local_type,
            action: None,
            done: false,
        });
        drop(st);
        let _ = session.connection.flush();
        wake(session);
        Ok(DragId(id))
    }

    pub(super) fn activation_token(window: &Window) -> Result<String> {
        let session = SESSION.lock();
        let session = session
            .as_ref()
            .ok_or_else(|| Error::new("Wayland connection is unavailable"))?;
        let pending = Arc::new(PendingToken::default());
        let token = {
            let state = session.state.lock();
            let activation = state
                .activation
                .as_ref()
                .ok_or_else(|| Error::new("the compositor does not support xdg-activation-v1"))?;
            let surface = surface_of(window)
                .and_then(|surface| window_surface_proxy(&session.connection, surface))
                .ok_or_else(|| Error::new("the window has no Wayland surface"))?;
            let token = activation.get_activation_token(&session.handle, pending.clone());
            token.set_surface(&surface);
            if state.activation_serial != 0
                && let Some(seat) = &state.seat
            {
                token.set_serial(state.activation_serial, seat);
            }
            token.commit();
            token
        };
        let _cleanup = scopeguard_token(&token);
        session
            .connection
            .flush()
            .map_err(|error| Error::new(error.to_string()))?;
        wake(session);
        let mut value = pending.value.lock();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while value.is_none() {
            let remaining = until.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::new("desktop activation token timed out"));
            }
            pending.ready.wait_for(&mut value, remaining);
        }
        let token = value.take().unwrap();
        if token.is_empty() || token.len() > 4096 || token.contains('\0') {
            return Err(Error::new("invalid desktop activation token"));
        }
        Ok(token)
    }
    fn scopeguard_token(token: &XdgActivationTokenV1) -> impl Drop + '_ {
        struct Destroy<'a>(&'a XdgActivationTokenV1);
        impl Drop for Destroy<'_> {
            fn drop(&mut self) {
                self.0.destroy();
            }
        }
        Destroy(token)
    }

    pub(super) fn activate(window: &Window, token: &str) -> Result<()> {
        let session = SESSION.lock();
        let session = session
            .as_ref()
            .ok_or_else(|| Error::new("Wayland connection is unavailable"))?;
        let state = session.state.lock();
        let activation = state
            .activation
            .as_ref()
            .ok_or_else(|| Error::new("the compositor does not support xdg-activation-v1"))?;
        let surface = surface_of(window)
            .and_then(|surface| window_surface_proxy(&session.connection, surface))
            .ok_or_else(|| Error::new("the window has no Wayland surface"))?;
        activation.activate(token.to_string(), &surface);
        session
            .connection
            .flush()
            .map_err(|error| Error::new(error.to_string()))?;
        Ok(())
    }

    fn window_surface_proxy(
        connection: &Connection,
        surface: u64,
    ) -> Option<wayland_client::protocol::wl_surface::WlSurface> {
        if surface == 0 {
            return None;
        }
        // SAFETY: surface is SDL's live wl_surface proxy on this connection.
        let id = unsafe {
            wayland_backend::client::ObjectId::from_ptr(
                wayland_client::protocol::wl_surface::WlSurface::interface(),
                surface as *mut _,
            )
        }
        .ok()?;
        wayland_client::protocol::wl_surface::WlSurface::from_id(connection, id).ok()
    }

    fn wake(session: &Session) {
        let _ = rustix::io::write(&session.wake, &1u64.to_ne_bytes());
    }

    /// Releases a drag that has ended.
    pub(super) fn reap() {
        let session = SESSION.lock();
        let Some(session) = session.as_ref() else {
            return;
        };
        let mut st = session.state.lock();
        let version = st.manager_version;
        if let Some(mut drag) = st.drag.take_if(|d| d.done) {
            release_offer(&mut drag);
            if let Some(device) = drag.device.take()
                && version >= 2
            {
                device.release();
            }
            drag.source.destroy();
        }
    }

    pub(super) fn shutdown() {
        let Some(mut session) = SESSION.lock().take() else {
            return;
        };
        session.stop.store(true, Ordering::Release);
        wake(&session);
        if let Some(thread) = session.thread.take() {
            let _ = thread.join();
        }
        let mut st = session.state.lock();
        if let Some(mut drag) = st.drag.take() {
            release_offer(&mut drag);
            drag.source.destroy();
        }
        if let Some(pointer) = st.pointer.take()
            && pointer.version() >= 3
        {
            pointer.release();
        }
        if let Some(keyboard) = st.keyboard.take()
            && keyboard.version() >= 3
        {
            keyboard.release();
        }
        st.manager.take();
        if let Some(activation) = st.activation.take() {
            activation.destroy();
        }
        if let Some(seat) = st.seat.take()
            && seat.version() >= 5
        {
            seat.release();
        }
        let _ = session.connection.flush();
        LOCAL_TEXTS.lock().clear();
        let _ = session.wake.as_raw_fd();
    }
}
