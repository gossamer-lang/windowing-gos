//! Synthesized input: events a program queues as if a device or the window
//! system had sent them, to drive a UI from tests or record-and-replay.

use std::collections::HashSet;
use std::ffi::{CString, c_char};

use parking_lot::Mutex;
use sdl3_sys::events::*;
use sdl3_sys::keyboard::SDL_KeyboardID;
use sdl3_sys::keycode::{
    SDL_KMOD_CAPS, SDL_KMOD_LALT, SDL_KMOD_LCTRL, SDL_KMOD_LGUI, SDL_KMOD_LSHIFT, SDL_KMOD_MODE,
    SDL_KMOD_NUM, SDL_KMOD_SCROLL, SDL_Keycode, SDL_Keymod,
};
use sdl3_sys::mouse::{
    SDL_BUTTON_LMASK, SDL_BUTTON_MMASK, SDL_BUTTON_RMASK, SDL_BUTTON_X1MASK, SDL_BUTTON_X2MASK,
    SDL_MOUSEWHEEL_NORMAL, SDL_MouseButtonFlags, SDL_MouseID,
};
use sdl3_sys::pen::SDL_PEN_MOUSEID;
use sdl3_sys::pen::{
    SDL_PEN_INPUT_BUTTON_1, SDL_PEN_INPUT_BUTTON_2, SDL_PEN_INPUT_BUTTON_3, SDL_PEN_INPUT_DOWN,
    SDL_PEN_INPUT_ERASER_TIP, SDL_PenAxis, SDL_PenID, SDL_PenInputFlags,
};
use sdl3_sys::scancode::SDL_Scancode;
use sdl3_sys::touch::{SDL_FingerID, SDL_TOUCH_MOUSEID, SDL_TouchID};
use sdl3_sys::video::{SDL_DisplayID, SDL_GetWindowFromID, SDL_GetWindowSize, SDL_WindowID};

use crate::display::{DisplayId, Orientation};
use crate::event::{Buttons, Event, Mods, MouseButton, PenAxis, PointerSource};
use crate::menu::MenuSource;
use crate::system::Theme;
use crate::window::WindowId;
use crate::{App, Result, dialog, event, support, waker};

fn wid(window: WindowId) -> SDL_WindowID {
    SDL_WindowID(window.0)
}

fn mods_bits(mods: Mods) -> SDL_Keymod {
    let pairs = [
        (mods.shift, SDL_KMOD_LSHIFT),
        (mods.ctrl, SDL_KMOD_LCTRL),
        (mods.alt, SDL_KMOD_LALT),
        (mods.gui, SDL_KMOD_LGUI),
        (mods.alt_gr, SDL_KMOD_MODE),
        (mods.caps_lock, SDL_KMOD_CAPS),
        (mods.num_lock, SDL_KMOD_NUM),
        (mods.scroll_lock, SDL_KMOD_SCROLL),
    ];
    SDL_Keymod(
        pairs
            .iter()
            .filter(|(on, _)| *on)
            .fold(0, |bits, (_, m)| bits | m.0),
    )
}

fn buttons_bits(buttons: Buttons) -> SDL_MouseButtonFlags {
    let pairs = [
        (buttons.left, SDL_BUTTON_LMASK),
        (buttons.middle, SDL_BUTTON_MMASK),
        (buttons.right, SDL_BUTTON_RMASK),
        (buttons.back, SDL_BUTTON_X1MASK),
        (buttons.forward, SDL_BUTTON_X2MASK),
    ];
    SDL_MouseButtonFlags(
        pairs
            .iter()
            .filter(|(on, _)| *on)
            .fold(0, |bits, (_, m)| bits | m.0),
    )
}

fn button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Back => 4,
        MouseButton::Forward => 5,
        MouseButton::Other(n) => n as u8,
    }
}

fn theme_code(theme: Theme) -> i32 {
    match theme {
        Theme::Unknown => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}

fn orientation_code(orientation: Orientation) -> i32 {
    match orientation {
        Orientation::Unknown => 0,
        Orientation::Landscape => 1,
        Orientation::LandscapeFlipped => 2,
        Orientation::Portrait => 3,
        Orientation::PortraitFlipped => 4,
    }
}

fn pen_axis_code(axis: PenAxis) -> i32 {
    match axis {
        PenAxis::Pressure => 0,
        PenAxis::TiltX => 1,
        PenAxis::TiltY => 2,
        PenAxis::Distance => 3,
        PenAxis::Rotation => 4,
        PenAxis::Slider => 5,
        PenAxis::TangentialPressure => 6,
        PenAxis::Other(n) => n as i32,
    }
}

fn source_id(source: PointerSource) -> SDL_MouseID {
    match source {
        PointerSource::Mouse => SDL_MouseID(0),
        PointerSource::Touch => SDL_TOUCH_MOUSEID,
        PointerSource::Pen => SDL_PEN_MOUSEID,
    }
}

// A touch event carries coordinates normalized to its window.
fn window_extent(window: WindowId) -> (f32, f32) {
    // SAFETY: SDL answers null for an unknown window.
    let handle = unsafe { SDL_GetWindowFromID(wid(window)) };
    if handle.is_null() {
        return (1.0, 1.0);
    }
    let (mut w, mut h) = (0, 0);
    // SAFETY: handle is live.
    unsafe { SDL_GetWindowSize(handle, &mut w, &mut h) };
    if w > 0 && h > 0 {
        (w as f32, h as f32)
    } else {
        (1.0, 1.0)
    }
}

// A pushed event's text, copied to memory the decoder frees. Only
// simulation allocations belong to this registry; SDL owns native payloads.
static INJECTED_TEXT: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

fn injected_text(text: &str) -> Result<*const c_char> {
    let copy = support::c_text(text, "simulated text")?.into_raw();
    INJECTED_TEXT
        .lock()
        .get_or_insert_with(HashSet::new)
        .insert(copy as usize);
    Ok(copy)
}

fn free_injected(address: usize) {
    if INJECTED_TEXT
        .lock()
        .as_mut()
        .is_some_and(|set| set.remove(&address))
    {
        // SAFETY: the address came from CString::into_raw and left the registry once.
        drop(unsafe { CString::from_raw(address as *mut c_char) });
    }
}

// An event SDL refused is never decoded, so its text copy is freed here.
fn keep_if_queued(pushed: Result<()>, copy: *const c_char) -> Result<()> {
    if pushed.is_err() {
        free_injected(copy as usize);
    }
    pushed
}

fn push(mut event: SDL_Event) -> Result<()> {
    // SAFETY: event is a complete union SDL copies into its queue.
    support::check(
        unsafe { SDL_PushEvent(&mut event) },
        "queueing a simulated event",
    )
}

fn zeroed() -> SDL_Event {
    // SAFETY: an all-zero SDL_Event is a valid value of the union.
    unsafe { std::mem::zeroed() }
}

fn common(kind: SDL_EventType) -> Result<()> {
    let mut e = zeroed();
    e.common = SDL_CommonEvent {
        r#type: kind.0,
        reserved: 0,
        timestamp: 0,
    };
    push(e)
}

fn window_event(kind: SDL_EventType, window: WindowId, data1: i64, data2: i64) -> Result<()> {
    let mut e = zeroed();
    e.window = SDL_WindowEvent {
        r#type: kind,
        reserved: 0,
        timestamp: 0,
        windowID: wid(window),
        data1: data1 as i32,
        data2: data2 as i32,
    };
    push(e)
}

fn display_event(kind: SDL_EventType, display: DisplayId, data1: i32) -> Result<()> {
    let mut e = zeroed();
    e.display = SDL_DisplayEvent {
        r#type: kind,
        reserved: 0,
        timestamp: 0,
        displayID: SDL_DisplayID(display.0),
        data1,
        data2: 0,
    };
    push(e)
}

fn pinch_event(kind: SDL_EventType, window: WindowId, scale: f64) -> Result<()> {
    let mut e = zeroed();
    e.pinch = SDL_PinchFingerEvent {
        r#type: kind,
        reserved: 0,
        timestamp: 0,
        scale: scale as f32,
        windowID: wid(window),
    };
    push(e)
}

fn pen_proximity(kind: SDL_EventType, window: WindowId, pen: u32) -> Result<()> {
    let mut e = zeroed();
    e.pproximity = SDL_PenProximityEvent {
        r#type: kind,
        reserved: 0,
        timestamp: 0,
        windowID: wid(window),
        which: SDL_PenID(pen),
    };
    push(e)
}

fn pen_button(down: bool, window: WindowId, pen: u32, button: i64, x: f64, y: f64) -> Result<()> {
    let mut e = zeroed();
    e.pbutton = SDL_PenButtonEvent {
        r#type: if down {
            SDL_EVENT_PEN_BUTTON_DOWN
        } else {
            SDL_EVENT_PEN_BUTTON_UP
        },
        reserved: 0,
        timestamp: 0,
        windowID: wid(window),
        which: SDL_PenID(pen),
        pen_state: SDL_PenInputFlags(0),
        x: x as f32,
        y: y as f32,
        button: button as u8,
        down,
    };
    push(e)
}

fn pen_touch(down: bool, window: WindowId, pen: u32, x: f64, y: f64, eraser: bool) -> Result<()> {
    let mut e = zeroed();
    e.ptouch = SDL_PenTouchEvent {
        r#type: if down {
            SDL_EVENT_PEN_DOWN
        } else {
            SDL_EVENT_PEN_UP
        },
        reserved: 0,
        timestamp: 0,
        windowID: wid(window),
        which: SDL_PenID(pen),
        pen_state: if down {
            SDL_PEN_INPUT_DOWN
        } else {
            SDL_PenInputFlags(0)
        },
        x: x as f32,
        y: y as f32,
        eraser,
        down,
    };
    push(e)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one argument per field of SDL's touch event"
)]
fn touch(
    kind: SDL_EventType,
    window: WindowId,
    device: u64,
    finger: u64,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
    pressure: f64,
) -> Result<()> {
    let (w, h) = window_extent(window);
    let mut e = zeroed();
    e.tfinger = SDL_TouchFingerEvent {
        r#type: kind,
        reserved: 0,
        timestamp: 0,
        touchID: SDL_TouchID(device),
        fingerID: SDL_FingerID(finger),
        x: x as f32 / w,
        y: y as f32 / h,
        dx: dx as f32 / w,
        dy: dy as f32 / h,
        pressure: pressure as f32,
        windowID: wid(window),
    };
    push(e)
}

fn drop_event(kind: SDL_EventType, window: WindowId, data: &str, x: f64, y: f64) -> Result<()> {
    let copy = injected_text(data)?;
    let mut e = zeroed();
    e.drop = SDL_DropEvent {
        r#type: kind,
        reserved: 0,
        timestamp: 0,
        windowID: wid(window),
        x: x as f32,
        y: y as f32,
        source: copy,
        data: copy,
    };
    keep_if_queued(push(e), copy)
}

impl App {
    /// Queues `event` as if the platform had delivered it.
    pub fn simulate(&self, event: Event) -> Result<()> {
        support::require_main_thread("App::simulate");
        let base = self.event_base;
        match event {
            Event::RenderReset { window } => {
                let mut e = zeroed();
                e.render = SDL_RenderEvent {
                    r#type: SDL_EVENT_RENDER_DEVICE_RESET,
                    windowID: wid(window),
                    ..Default::default()
                };
                push(e)
            }
            Event::QuitRequested => common(SDL_EVENT_QUIT),
            Event::KeyDown {
                window,
                key,
                scancode,
                mods,
                repeat,
            } => {
                let mut e = zeroed();
                e.key = SDL_KeyboardEvent {
                    r#type: SDL_EVENT_KEY_DOWN,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    which: SDL_KeyboardID(0),
                    scancode: SDL_Scancode(scancode.code()),
                    key: SDL_Keycode(key.code()),
                    r#mod: mods_bits(mods),
                    raw: 0,
                    down: true,
                    repeat,
                };
                push(e)
            }
            Event::KeyUp {
                window,
                key,
                scancode,
                mods,
            } => {
                let mut e = zeroed();
                e.key = SDL_KeyboardEvent {
                    r#type: SDL_EVENT_KEY_UP,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    which: SDL_KeyboardID(0),
                    scancode: SDL_Scancode(scancode.code()),
                    key: SDL_Keycode(key.code()),
                    r#mod: mods_bits(mods),
                    raw: 0,
                    down: false,
                    repeat: false,
                };
                push(e)
            }
            Event::Text { window, text } => {
                let copy = injected_text(&text)?;
                let mut e = zeroed();
                e.text = SDL_TextInputEvent {
                    r#type: SDL_EVENT_TEXT_INPUT,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    text: copy,
                };
                keep_if_queued(push(e), copy)
            }
            Event::Composing {
                window,
                text,
                cursor,
                selection,
            } => {
                let copy = injected_text(&text)?;
                let mut e = zeroed();
                e.edit = SDL_TextEditingEvent {
                    r#type: SDL_EVENT_TEXT_EDITING,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    text: copy,
                    start: cursor as i32,
                    length: selection as i32,
                };
                keep_if_queued(push(e), copy)
            }
            Event::PointerMoved {
                window,
                x,
                y,
                dx,
                dy,
                buttons,
                source,
            } => {
                let mut e = zeroed();
                e.motion = SDL_MouseMotionEvent {
                    r#type: SDL_EVENT_MOUSE_MOTION,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    which: source_id(source),
                    state: buttons_bits(buttons),
                    x: x as f32,
                    y: y as f32,
                    xrel: dx as f32,
                    yrel: dy as f32,
                };
                push(e)
            }
            Event::ButtonDown {
                window,
                button,
                x,
                y,
                clicks,
                source,
            }
            | Event::ButtonUp {
                window,
                button,
                x,
                y,
                clicks,
                source,
            } => {
                let down = matches!(event, Event::ButtonDown { .. });
                let mut e = zeroed();
                e.button = SDL_MouseButtonEvent {
                    r#type: if down {
                        SDL_EVENT_MOUSE_BUTTON_DOWN
                    } else {
                        SDL_EVENT_MOUSE_BUTTON_UP
                    },
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    which: source_id(source),
                    button: button_code(button),
                    down,
                    clicks: clicks as u8,
                    x: x as f32,
                    y: y as f32,
                    ..Default::default()
                };
                push(e)
            }
            Event::Wheel {
                window,
                dx,
                dy,
                x,
                y,
            } => {
                let mut e = zeroed();
                e.wheel = SDL_MouseWheelEvent {
                    r#type: SDL_EVENT_MOUSE_WHEEL,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    which: SDL_MouseID(0),
                    x: dx as f32,
                    y: dy as f32,
                    direction: SDL_MOUSEWHEEL_NORMAL,
                    mouse_x: x as f32,
                    mouse_y: y as f32,
                    integer_x: dx as i32,
                    integer_y: dy as i32,
                };
                push(e)
            }
            Event::TouchDown {
                window,
                device,
                finger,
                x,
                y,
                pressure,
            } => touch(
                SDL_EVENT_FINGER_DOWN,
                window,
                device,
                finger,
                x,
                y,
                0.0,
                0.0,
                pressure,
            ),
            Event::TouchMoved {
                window,
                device,
                finger,
                x,
                y,
                dx,
                dy,
                pressure,
            } => touch(
                SDL_EVENT_FINGER_MOTION,
                window,
                device,
                finger,
                x,
                y,
                dx,
                dy,
                pressure,
            ),
            Event::TouchUp {
                window,
                device,
                finger,
                x,
                y,
            } => touch(
                SDL_EVENT_FINGER_UP,
                window,
                device,
                finger,
                x,
                y,
                0.0,
                0.0,
                0.0,
            ),
            Event::TouchCancelled {
                window,
                device,
                finger,
            } => touch(
                SDL_EVENT_FINGER_CANCELED,
                window,
                device,
                finger,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
            ),
            Event::PenDown {
                window,
                pen,
                x,
                y,
                eraser,
            } => pen_touch(true, window, pen, x, y, eraser),
            Event::PenUp {
                window,
                pen,
                x,
                y,
                eraser,
            } => pen_touch(false, window, pen, x, y, eraser),
            Event::PenMoved {
                window,
                pen,
                x,
                y,
                state,
            } => {
                let pairs = [
                    (state.down, SDL_PEN_INPUT_DOWN),
                    (state.eraser, SDL_PEN_INPUT_ERASER_TIP),
                    (state.primary_button, SDL_PEN_INPUT_BUTTON_1),
                    (state.secondary_button, SDL_PEN_INPUT_BUTTON_2),
                    (state.tertiary_button, SDL_PEN_INPUT_BUTTON_3),
                ];
                let bits = pairs
                    .iter()
                    .filter(|(on, _)| *on)
                    .fold(0, |b, (_, f)| b | f.0);
                let mut e = zeroed();
                e.pmotion = SDL_PenMotionEvent {
                    r#type: SDL_EVENT_PEN_MOTION,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    which: SDL_PenID(pen),
                    pen_state: SDL_PenInputFlags(bits),
                    x: x as f32,
                    y: y as f32,
                };
                push(e)
            }
            Event::DropBegan { window } => drop_event(SDL_EVENT_DROP_BEGIN, window, "", 0.0, 0.0),
            Event::DropMoved { window, x, y } => {
                drop_event(SDL_EVENT_DROP_POSITION, window, "", x, y)
            }
            Event::DroppedFile { window, path, x, y } => {
                drop_event(SDL_EVENT_DROP_FILE, window, &path, x, y)
            }
            Event::OpenRequested { path } => {
                drop_event(SDL_EVENT_DROP_FILE, WindowId(0), &path, 0.0, 0.0)
            }
            Event::DroppedText { window, text, x, y } => {
                drop_event(SDL_EVENT_DROP_TEXT, window, &text, x, y)
            }
            Event::DropEnded { window, x, y } => {
                drop_event(SDL_EVENT_DROP_COMPLETE, window, "", x, y)
            }
            Event::CloseRequested { window } => {
                window_event(SDL_EVENT_WINDOW_CLOSE_REQUESTED, window, 0, 0)
            }
            Event::Resized {
                window,
                width,
                height,
            } => window_event(SDL_EVENT_WINDOW_RESIZED, window, width, height),
            Event::Moved { window, x, y } => window_event(SDL_EVENT_WINDOW_MOVED, window, x, y),
            Event::FocusGained { window } => {
                window_event(SDL_EVENT_WINDOW_FOCUS_GAINED, window, 0, 0)
            }
            Event::FocusLost { window } => window_event(SDL_EVENT_WINDOW_FOCUS_LOST, window, 0, 0),
            Event::PointerEntered { window } => {
                window_event(SDL_EVENT_WINDOW_MOUSE_ENTER, window, 0, 0)
            }
            Event::PointerLeft { window } => {
                window_event(SDL_EVENT_WINDOW_MOUSE_LEAVE, window, 0, 0)
            }
            Event::Minimized { window } => window_event(SDL_EVENT_WINDOW_MINIMIZED, window, 0, 0),
            Event::Maximized { window } => window_event(SDL_EVENT_WINDOW_MAXIMIZED, window, 0, 0),
            Event::Restored { window } => window_event(SDL_EVENT_WINDOW_RESTORED, window, 0, 0),
            Event::WindowShown { window } => window_event(SDL_EVENT_WINDOW_SHOWN, window, 0, 0),
            Event::WindowHidden { window } => window_event(SDL_EVENT_WINDOW_HIDDEN, window, 0, 0),
            Event::RedrawRequested { window } => {
                waker::push_user_event(base + event::REDRAW_OFFSET, window.0, 0, 0, 0)
            }
            Event::Wake => self.waker().wake(),
            Event::Terminating => common(SDL_EVENT_TERMINATING),
            Event::LowMemory => common(SDL_EVENT_LOW_MEMORY),
            Event::Suspending => common(SDL_EVENT_WILL_ENTER_BACKGROUND),
            Event::Suspended => common(SDL_EVENT_DID_ENTER_BACKGROUND),
            Event::Resuming => common(SDL_EVENT_WILL_ENTER_FOREGROUND),
            Event::Resumed => common(SDL_EVENT_DID_ENTER_FOREGROUND),
            Event::LocaleChanged => common(SDL_EVENT_LOCALE_CHANGED),
            Event::KeymapChanged => common(SDL_EVENT_KEYMAP_CHANGED),
            Event::ThemeChanged { theme } => {
                waker::push_user_event(base + event::THEME_OFFSET, 0, theme_code(theme), 0, 0)
            }
            Event::ClipboardChanged { owned } => {
                let mut e = zeroed();
                e.clipboard = SDL_ClipboardEvent {
                    r#type: SDL_EVENT_CLIPBOARD_UPDATE,
                    reserved: 0,
                    timestamp: 0,
                    owner: owned,
                    num_mime_types: 0,
                    mime_types: std::ptr::null_mut(),
                };
                push(e)
            }
            Event::DialogClosed { dialog, choice } => {
                dialog::queue_choice(base + event::DIALOG_OFFSET, dialog, choice)
            }
            Event::MenuSelected { source, item } => {
                let (window, tray) = match source {
                    MenuSource::MenuBar { window } => (window.map_or(0, |w| w.0), 0u64),
                    MenuSource::Tray { tray } => (0, u64::from(tray.0)),
                };
                waker::push_user_event(base + event::MENU_OFFSET, window, item as i32, tray, 0)
            }
            Event::DragFinished { drag, operation } => waker::push_user_event(
                base + event::DRAG_OFFSET,
                0,
                crate::drag::operation_code(operation),
                drag.0,
                0,
            ),
            Event::PreferencesChanged => {
                waker::push_user_event(base + event::PREFERENCES_OFFSET, 0, 0, 0, 0)
            }
            Event::DisplayAdded { display } => display_event(SDL_EVENT_DISPLAY_ADDED, display, 0),
            Event::DisplayRemoved { display } => {
                display_event(SDL_EVENT_DISPLAY_REMOVED, display, 0)
            }
            Event::DisplayMoved { display } => display_event(SDL_EVENT_DISPLAY_MOVED, display, 0),
            Event::DisplayModeChanged { display } => {
                display_event(SDL_EVENT_DISPLAY_CURRENT_MODE_CHANGED, display, 0)
            }
            Event::DisplayScaleChanged { display } => {
                display_event(SDL_EVENT_DISPLAY_CONTENT_SCALE_CHANGED, display, 0)
            }
            Event::DisplayUsableBoundsChanged { display } => {
                display_event(SDL_EVENT_DISPLAY_USABLE_BOUNDS_CHANGED, display, 0)
            }
            Event::DisplayOrientationChanged {
                display,
                orientation,
            } => display_event(
                SDL_EVENT_DISPLAY_ORIENTATION,
                display,
                orientation_code(orientation),
            ),
            Event::PixelSizeChanged {
                window,
                width,
                height,
            } => window_event(SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED, window, width, height),
            Event::ScaleChanged { window, scale } => {
                waker::push_user_event(base + event::SCALE_OFFSET, window.0, 0, scale.to_bits(), 0)
            }
            Event::DisplayChanged { window, display } => window_event(
                SDL_EVENT_WINDOW_DISPLAY_CHANGED,
                window,
                i64::from(display.0),
                0,
            ),
            Event::Occluded { window } => window_event(SDL_EVENT_WINDOW_OCCLUDED, window, 0, 0),
            Event::EnteredFullscreen { window } => {
                window_event(SDL_EVENT_WINDOW_ENTER_FULLSCREEN, window, 0, 0)
            }
            Event::LeftFullscreen { window } => {
                window_event(SDL_EVENT_WINDOW_LEAVE_FULLSCREEN, window, 0, 0)
            }
            Event::PinchBegan { window } => pinch_event(SDL_EVENT_PINCH_BEGIN, window, 1.0),
            Event::Pinch { window, scale } => pinch_event(SDL_EVENT_PINCH_UPDATE, window, scale),
            Event::PinchEnded { window } => pinch_event(SDL_EVENT_PINCH_END, window, 1.0),
            Event::PenEntered { window, pen } => {
                pen_proximity(SDL_EVENT_PEN_PROXIMITY_IN, window, pen)
            }
            Event::PenLeft { window, pen } => {
                pen_proximity(SDL_EVENT_PEN_PROXIMITY_OUT, window, pen)
            }
            Event::PenButtonDown {
                window,
                pen,
                button,
                x,
                y,
            } => pen_button(true, window, pen, button, x, y),
            Event::PenButtonUp {
                window,
                pen,
                button,
                x,
                y,
            } => pen_button(false, window, pen, button, x, y),
            Event::PenAxisChanged {
                window,
                pen,
                axis,
                value,
                x,
                y,
            } => {
                let mut e = zeroed();
                e.paxis = SDL_PenAxisEvent {
                    r#type: SDL_EVENT_PEN_AXIS,
                    reserved: 0,
                    timestamp: 0,
                    windowID: wid(window),
                    which: SDL_PenID(pen),
                    pen_state: SDL_PenInputFlags(0),
                    x: x as f32,
                    y: y as f32,
                    axis: SDL_PenAxis(pen_axis_code(axis)),
                    value: value as f32,
                };
                push(e)
            }
        }
    }
}

/// Frees every simulated text the loop has not yet decoded.
pub(crate) fn release_all_injected() {
    let drained: Vec<usize> = INJECTED_TEXT
        .lock()
        .as_mut()
        .map(|set| set.drain().collect())
        .unwrap_or_default();
    for address in drained {
        // SAFETY: each address came from CString::into_raw and is freed once.
        drop(unsafe { CString::from_raw(address as *mut c_char) });
    }
}

/// Frees the simulated text `e` carried, now that it has been decoded.
pub(crate) fn release_injected(e: &SDL_Event) {
    // SAFETY: each arm reads the member the event type names.
    let address = unsafe {
        match SDL_EventType(e.r#type) {
            SDL_EVENT_TEXT_INPUT => e.text.text as usize,
            SDL_EVENT_TEXT_EDITING => e.edit.text as usize,
            SDL_EVENT_DROP_FILE
            | SDL_EVENT_DROP_TEXT
            | SDL_EVENT_DROP_BEGIN
            | SDL_EVENT_DROP_COMPLETE
            | SDL_EVENT_DROP_POSITION => e.drop.data as usize,
            _ => return,
        }
    };
    free_injected(address);
}
