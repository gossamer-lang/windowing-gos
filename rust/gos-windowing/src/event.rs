//! Events, and the decoder that turns each SDL event the library models into
//! one.

use sdl3_sys::events::*;
use sdl3_sys::keycode::{
    SDL_KMOD_CAPS, SDL_KMOD_LALT, SDL_KMOD_LCTRL, SDL_KMOD_LGUI, SDL_KMOD_LSHIFT, SDL_KMOD_MODE,
    SDL_KMOD_NUM, SDL_KMOD_RALT, SDL_KMOD_RCTRL, SDL_KMOD_RGUI, SDL_KMOD_RSHIFT, SDL_KMOD_SCROLL,
    SDL_Keymod,
};
use sdl3_sys::mouse::{
    SDL_BUTTON_LMASK, SDL_BUTTON_MMASK, SDL_BUTTON_RMASK, SDL_BUTTON_X1MASK, SDL_BUTTON_X2MASK,
    SDL_MOUSEWHEEL_FLIPPED, SDL_MouseButtonFlags,
};
use sdl3_sys::pen::SDL_PEN_MOUSEID;
use sdl3_sys::pen::{
    SDL_PEN_INPUT_BUTTON_1, SDL_PEN_INPUT_BUTTON_2, SDL_PEN_INPUT_BUTTON_3, SDL_PEN_INPUT_DOWN,
    SDL_PEN_INPUT_ERASER_TIP, SDL_PenInputFlags,
};
use sdl3_sys::touch::SDL_TOUCH_MOUSEID;
use sdl3_sys::video::SDL_GetSystemTheme;
use sdl3_sys::video::{
    SDL_GetWindowDisplayScale, SDL_GetWindowFromID, SDL_GetWindowSize, SDL_WindowID,
};

use crate::dialog::{DialogChoice, DialogId};
use crate::display::{DisplayId, Orientation, orientation_from};
use crate::drag::{DragId, DragOperation};
use crate::instance::native_path;
use crate::keys::{Key, Scancode};
use crate::menu::{MenuSource, TrayId};
use crate::support;
use crate::system::{Theme, theme_from};
use crate::window::WindowId;

/// The modifier keys held during a key or pointer event. `gui` is the
/// Windows key, Command on macOS, Super elsewhere.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub gui: bool,
    /// AltGr, the third-level shift some layouts type with.
    pub alt_gr: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub scroll_lock: bool,
}

impl Mods {
    /// The modifier a platform's shortcuts use: Command on macOS, Ctrl
    /// everywhere else.
    pub fn shortcut(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.gui
        } else {
            self.ctrl
        }
    }
}

/// The modifiers SDL's bit set `raw` holds.
pub(crate) fn mods_from(raw: SDL_Keymod) -> Mods {
    let has = |bits: SDL_Keymod| raw.0 & bits.0 != 0;
    Mods {
        shift: has(SDL_KMOD_LSHIFT) || has(SDL_KMOD_RSHIFT),
        ctrl: has(SDL_KMOD_LCTRL) || has(SDL_KMOD_RCTRL),
        alt: has(SDL_KMOD_LALT) || has(SDL_KMOD_RALT),
        gui: has(SDL_KMOD_LGUI) || has(SDL_KMOD_RGUI),
        alt_gr: has(SDL_KMOD_MODE),
        caps_lock: has(SDL_KMOD_CAPS),
        num_lock: has(SDL_KMOD_NUM),
        scroll_lock: has(SDL_KMOD_SCROLL),
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// The first side button, usually "back".
    Back,
    /// The second side button, usually "forward".
    Forward,
    Other(i64),
}

/// The mouse buttons held during a motion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Buttons {
    pub left: bool,
    pub middle: bool,
    pub right: bool,
    pub back: bool,
    pub forward: bool,
}

/// What produced a pointer event: a mouse, or a touch or pen the platform
/// also reports as the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PointerSource {
    Mouse,
    Touch,
    Pen,
}

/// A pen's state during a pen event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PenState {
    /// Whether the tip touches the surface.
    pub down: bool,
    /// Whether the eraser end is the one in use.
    pub eraser: bool,
    /// The barrel buttons held, first to third.
    pub primary_button: bool,
    pub secondary_button: bool,
    pub tertiary_button: bool,
}

/// A pen axis a `PenAxisChanged` event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PenAxis {
    /// Tip pressure, 0 to 1.
    Pressure,
    /// Tilt along the x axis in degrees, -90 to 90.
    TiltX,
    /// Tilt along the y axis in degrees, -90 to 90.
    TiltY,
    /// Distance from the surface, 0 to 1.
    Distance,
    /// Barrel rotation in degrees, -180 to 180.
    Rotation,
    /// A finger wheel or slider, 0 to 1.
    Slider,
    /// Pressure on a tangential sensor, 0 to 1.
    TangentialPressure,
    Other(i64),
}

/// Something that happened to the application, a window, a display, or an
/// input device. `window` is the window the event belongs to; an input
/// event no window has focus for carries the id 0, which no window has.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Event {
    /// The user or the system asked the application to quit.
    QuitRequested,
    /// The system is ending the application now (mobile platforms).
    Terminating,
    /// The system is short of memory; free what can be freed.
    LowMemory,
    /// The application is about to move to the background.
    Suspending,
    /// The application is in the background.
    Suspended,
    /// The application is about to return to the foreground.
    Resuming,
    /// The application is in the foreground again.
    Resumed,
    LocaleChanged,
    ThemeChanged {
        theme: Theme,
    },
    /// The keyboard layout changed, so keys map to other characters.
    KeymapChanged,
    /// The clipboard's contents changed; `owned` when this application set them.
    ClipboardChanged {
        owned: bool,
    },
    /// A `Waker` woke the loop.
    Wake,
    /// A window should be drawn: it was exposed, or `request_redraw` asked.
    /// GPU device or render targets were lost; recreate renderer resources.
    RenderReset {
        window: WindowId,
    },
    RedrawRequested {
        window: WindowId,
    },
    /// A file or folder dialog the application opened was closed.
    DialogClosed {
        dialog: DialogId,
        choice: DialogChoice,
    },
    DisplayAdded {
        display: DisplayId,
    },
    DisplayRemoved {
        display: DisplayId,
    },
    DisplayMoved {
        display: DisplayId,
    },
    DisplayModeChanged {
        display: DisplayId,
    },
    DisplayScaleChanged {
        display: DisplayId,
    },
    DisplayUsableBoundsChanged {
        display: DisplayId,
    },
    DisplayOrientationChanged {
        display: DisplayId,
        orientation: Orientation,
    },
    WindowShown {
        window: WindowId,
    },
    WindowHidden {
        window: WindowId,
    },
    /// The window moved; its position in logical desktop coordinates.
    Moved {
        window: WindowId,
        x: i64,
        y: i64,
    },
    /// The window's logical size changed.
    Resized {
        window: WindowId,
        width: i64,
        height: i64,
    },
    /// The window's size in pixels changed (a resize, or a scale change).
    PixelSizeChanged {
        window: WindowId,
        width: i64,
        height: i64,
    },
    /// The window's scale changed, usually by moving to another display.
    ScaleChanged {
        window: WindowId,
        scale: f64,
    },
    Minimized {
        window: WindowId,
    },
    Maximized {
        window: WindowId,
    },
    Restored {
        window: WindowId,
    },
    PointerEntered {
        window: WindowId,
    },
    PointerLeft {
        window: WindowId,
    },
    FocusGained {
        window: WindowId,
    },
    FocusLost {
        window: WindowId,
    },
    /// The user asked to close the window. It stays open until closed.
    CloseRequested {
        window: WindowId,
    },
    DisplayChanged {
        window: WindowId,
        display: DisplayId,
    },
    /// The window is fully covered and need not be drawn.
    Occluded {
        window: WindowId,
    },
    EnteredFullscreen {
        window: WindowId,
    },
    LeftFullscreen {
        window: WindowId,
    },
    /// A key went down. `key` is what the layout names it, for shortcuts;
    /// `scancode` its position. Text never comes from key events: see `Text`.
    KeyDown {
        window: WindowId,
        key: Key,
        scancode: Scancode,
        mods: Mods,
        repeat: bool,
    },
    KeyUp {
        window: WindowId,
        key: Key,
        scancode: Scancode,
        mods: Mods,
    },
    /// Text the user committed. Arrives only while the window takes text input.
    Text {
        window: WindowId,
        text: String,
    },
    /// Input-method text being composed and not yet committed. `cursor` is
    /// the caret's character offset in `text`, `selection` the length of
    /// the selected run after it.
    Composing {
        window: WindowId,
        text: String,
        cursor: i64,
        selection: i64,
    },
    /// The pointer moved, to `x`, `y` in the window's logical coordinates,
    /// by `dx`, `dy`.
    PointerMoved {
        window: WindowId,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        buttons: Buttons,
        source: PointerSource,
    },
    ButtonDown {
        window: WindowId,
        button: MouseButton,
        x: f64,
        y: f64,
        clicks: i64,
        source: PointerSource,
    },
    ButtonUp {
        window: WindowId,
        button: MouseButton,
        x: f64,
        y: f64,
        clicks: i64,
        source: PointerSource,
    },
    /// A scroll: `dx`, `dy` in wheel notches, positive away from the user
    /// and to the right whatever the natural-scrolling setting.
    Wheel {
        window: WindowId,
        dx: f64,
        dy: f64,
        x: f64,
        y: f64,
    },
    TouchDown {
        window: WindowId,
        device: u64,
        finger: u64,
        x: f64,
        y: f64,
        pressure: f64,
    },
    TouchMoved {
        window: WindowId,
        device: u64,
        finger: u64,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        pressure: f64,
    },
    TouchUp {
        window: WindowId,
        device: u64,
        finger: u64,
        x: f64,
        y: f64,
    },
    /// The system took a touch over (a gesture of its own); treat it as up.
    TouchCancelled {
        window: WindowId,
        device: u64,
        finger: u64,
    },
    PinchBegan {
        window: WindowId,
    },
    /// A pinch gesture; `scale` relative to where it began.
    Pinch {
        window: WindowId,
        scale: f64,
    },
    PinchEnded {
        window: WindowId,
    },
    PenEntered {
        window: WindowId,
        pen: u32,
    },
    PenLeft {
        window: WindowId,
        pen: u32,
    },
    PenDown {
        window: WindowId,
        pen: u32,
        x: f64,
        y: f64,
        eraser: bool,
    },
    PenUp {
        window: WindowId,
        pen: u32,
        x: f64,
        y: f64,
        eraser: bool,
    },
    PenMoved {
        window: WindowId,
        pen: u32,
        x: f64,
        y: f64,
        state: PenState,
    },
    PenButtonDown {
        window: WindowId,
        pen: u32,
        button: i64,
        x: f64,
        y: f64,
    },
    PenButtonUp {
        window: WindowId,
        pen: u32,
        button: i64,
        x: f64,
        y: f64,
    },
    PenAxisChanged {
        window: WindowId,
        pen: u32,
        axis: PenAxis,
        value: f64,
        x: f64,
        y: f64,
    },
    /// A drag entered the window.
    DropBegan {
        window: WindowId,
    },
    /// A drag moved over the window.
    DropMoved {
        window: WindowId,
        x: f64,
        y: f64,
    },
    /// A file was dropped; one event per file.
    DroppedFile {
        window: WindowId,
        path: String,
        x: f64,
        y: f64,
    },
    DroppedText {
        window: WindowId,
        text: String,
        x: f64,
        y: f64,
    },
    /// The drag left the window or every item was dropped.
    DropEnded {
        window: WindowId,
        x: f64,
        y: f64,
    },
    /// The system asked the running application to open `path`, outside any window.
    OpenRequested {
        path: String,
    },
    /// The user chose the native menu item `item` (the program's identity).
    MenuSelected {
        source: MenuSource,
        item: u32,
    },
    /// A drag `App::start_drag` began ended; `operation` is what the target
    /// did, `None` when nothing accepted the drop or it was cancelled.
    DragFinished {
        drag: DragId,
        operation: Option<DragOperation>,
    },
    /// The desktop's contrast, motion, accent, or text-size preferences may
    /// have changed; `system::preferences()` reads them.
    PreferencesChanged,
}

/// The event types the library registers with SDL, offsets from the first.
pub(crate) const WAKE_OFFSET: u32 = 0;
pub(crate) const REDRAW_OFFSET: u32 = 1;
pub(crate) const DIALOG_OFFSET: u32 = 2;
pub(crate) const THEME_OFFSET: u32 = 3;
pub(crate) const SCALE_OFFSET: u32 = 4;
pub(crate) const MENU_OFFSET: u32 = 5;
pub(crate) const DRAG_OFFSET: u32 = 6;
pub(crate) const LOCAL_DROP_OFFSET: u32 = 7;
pub(crate) const PREFERENCES_OFFSET: u32 = 8;
pub(crate) const USER_EVENT_COUNT: i32 = 9;

fn window_id(raw: SDL_WindowID) -> WindowId {
    WindowId(raw.0)
}

fn is(kind: u32, t: SDL_EventType) -> bool {
    kind == t.0
}

/// The event in `e`, or `None` for one the library does not model. `base`
/// is the first event type the library registered.
pub(crate) fn decode(e: &SDL_Event, base: u32) -> Option<Event> {
    // SAFETY: every member of the union starts with the type word, and each
    // branch below reads only the member that type names.
    unsafe {
        let kind = e.r#type;
        if kind >= base && kind < base + USER_EVENT_COUNT as u32 {
            return decode_own(&e.user, kind - base);
        }
        if (SDL_EVENT_WINDOW_SHOWN.0..=SDL_EVENT_WINDOW_DESTROYED.0).contains(&kind) {
            return decode_window(&e.window);
        }
        if (SDL_EVENT_DISPLAY_ORIENTATION.0..=SDL_EVENT_DISPLAY_USABLE_BOUNDS_CHANGED.0)
            .contains(&kind)
        {
            return decode_display(&e.display);
        }
        let t = SDL_EventType(kind);
        match t {
            SDL_EVENT_RENDER_TARGETS_RESET
            | SDL_EVENT_RENDER_DEVICE_RESET
            | SDL_EVENT_RENDER_DEVICE_LOST => Some(Event::RenderReset {
                window: window_id(e.render.windowID),
            }),
            SDL_EVENT_QUIT => Some(Event::QuitRequested),
            SDL_EVENT_TERMINATING => Some(Event::Terminating),
            SDL_EVENT_LOW_MEMORY => Some(Event::LowMemory),
            SDL_EVENT_WILL_ENTER_BACKGROUND => Some(Event::Suspending),
            SDL_EVENT_DID_ENTER_BACKGROUND => Some(Event::Suspended),
            SDL_EVENT_WILL_ENTER_FOREGROUND => Some(Event::Resuming),
            SDL_EVENT_DID_ENTER_FOREGROUND => Some(Event::Resumed),
            SDL_EVENT_LOCALE_CHANGED => Some(Event::LocaleChanged),
            SDL_EVENT_SYSTEM_THEME_CHANGED => Some(Event::ThemeChanged {
                theme: theme_from(SDL_GetSystemTheme().0),
            }),
            SDL_EVENT_KEYMAP_CHANGED => Some(Event::KeymapChanged),
            SDL_EVENT_CLIPBOARD_UPDATE => Some(Event::ClipboardChanged {
                owned: e.clipboard.owner,
            }),
            SDL_EVENT_KEY_DOWN | SDL_EVENT_KEY_UP => decode_key(&e.key),
            SDL_EVENT_TEXT_INPUT => Some(Event::Text {
                window: window_id(e.text.windowID),
                text: support::read_text(e.text.text),
            }),
            SDL_EVENT_TEXT_EDITING => Some(Event::Composing {
                window: window_id(e.edit.windowID),
                text: support::read_text(e.edit.text),
                cursor: i64::from(e.edit.start),
                selection: i64::from(e.edit.length),
            }),
            SDL_EVENT_MOUSE_MOTION => decode_motion(&e.motion),
            SDL_EVENT_MOUSE_BUTTON_DOWN | SDL_EVENT_MOUSE_BUTTON_UP => decode_button(&e.button),
            SDL_EVENT_MOUSE_WHEEL => decode_wheel(&e.wheel),
            SDL_EVENT_FINGER_DOWN
            | SDL_EVENT_FINGER_UP
            | SDL_EVENT_FINGER_MOTION
            | SDL_EVENT_FINGER_CANCELED => decode_touch(&e.tfinger),
            SDL_EVENT_PINCH_BEGIN | SDL_EVENT_PINCH_UPDATE | SDL_EVENT_PINCH_END => {
                decode_pinch(&e.pinch)
            }
            SDL_EVENT_DROP_FILE
            | SDL_EVENT_DROP_TEXT
            | SDL_EVENT_DROP_BEGIN
            | SDL_EVENT_DROP_COMPLETE
            | SDL_EVENT_DROP_POSITION => decode_drop(&e.drop),
            SDL_EVENT_PEN_PROXIMITY_IN | SDL_EVENT_PEN_PROXIMITY_OUT => {
                let p = &e.pproximity;
                let window = window_id(p.windowID);
                if is(kind, SDL_EVENT_PEN_PROXIMITY_IN) {
                    Some(Event::PenEntered {
                        window,
                        pen: p.which.0,
                    })
                } else {
                    Some(Event::PenLeft {
                        window,
                        pen: p.which.0,
                    })
                }
            }
            SDL_EVENT_PEN_DOWN | SDL_EVENT_PEN_UP => {
                let p = &e.ptouch;
                let window = window_id(p.windowID);
                let (x, y) = (f64::from(p.x), f64::from(p.y));
                if p.down {
                    Some(Event::PenDown {
                        window,
                        pen: p.which.0,
                        x,
                        y,
                        eraser: p.eraser,
                    })
                } else {
                    Some(Event::PenUp {
                        window,
                        pen: p.which.0,
                        x,
                        y,
                        eraser: p.eraser,
                    })
                }
            }
            SDL_EVENT_PEN_MOTION => {
                let p = &e.pmotion;
                Some(Event::PenMoved {
                    window: window_id(p.windowID),
                    pen: p.which.0,
                    x: f64::from(p.x),
                    y: f64::from(p.y),
                    state: pen_state_from(p.pen_state),
                })
            }
            SDL_EVENT_PEN_BUTTON_DOWN | SDL_EVENT_PEN_BUTTON_UP => {
                let p = &e.pbutton;
                let window = window_id(p.windowID);
                let button = i64::from(p.button);
                let (x, y) = (f64::from(p.x), f64::from(p.y));
                if p.down {
                    Some(Event::PenButtonDown {
                        window,
                        pen: p.which.0,
                        button,
                        x,
                        y,
                    })
                } else {
                    Some(Event::PenButtonUp {
                        window,
                        pen: p.which.0,
                        button,
                        x,
                        y,
                    })
                }
            }
            SDL_EVENT_PEN_AXIS => {
                let p = &e.paxis;
                Some(Event::PenAxisChanged {
                    window: window_id(p.windowID),
                    pen: p.which.0,
                    axis: pen_axis_from(p.axis.0),
                    value: f64::from(p.value),
                    x: f64::from(p.x),
                    y: f64::from(p.y),
                })
            }
            _ => None,
        }
    }
}

fn decode_own(e: &SDL_UserEvent, offset: u32) -> Option<Event> {
    let window = WindowId(e.windowID.0);
    let data1 = e.data1 as u64;
    let data2 = e.data2 as u64;
    match offset {
        WAKE_OFFSET => Some(Event::Wake),
        REDRAW_OFFSET => Some(Event::RedrawRequested { window }),
        THEME_OFFSET => Some(Event::ThemeChanged {
            theme: theme_from(e.code),
        }),
        SCALE_OFFSET => Some(Event::ScaleChanged {
            window,
            scale: f64::from_bits(data1),
        }),
        DIALOG_OFFSET => Some(Event::DialogClosed {
            dialog: DialogId(data2),
            choice: crate::dialog::take_choice(data1),
        }),
        // `data1` holds the tray identity, zero for the menu bar.
        MENU_OFFSET => {
            let source = if data1 == 0 {
                MenuSource::MenuBar {
                    window: (window.0 != 0).then_some(window),
                }
            } else {
                MenuSource::Tray {
                    tray: TrayId(data1 as u32),
                }
            };
            Some(Event::MenuSelected {
                source,
                item: e.code as u32,
            })
        }
        DRAG_OFFSET => {
            crate::drag::finished();
            Some(Event::DragFinished {
                drag: DragId(data1),
                operation: crate::drag::operation_from(e.code),
            })
        }
        PREFERENCES_OFFSET => Some(Event::PreferencesChanged),
        // A drop of the library's own Wayland drag on one of its windows,
        // reported as SDL reports a drop; `code` names the step.
        LOCAL_DROP_OFFSET => {
            let x = f64::from_bits(data1);
            let y = f64::from_bits(data2);
            match e.code {
                0 => Some(Event::DropBegan { window }),
                1 => Some(Event::DropMoved { window, x, y }),
                2 => Some(Event::DroppedText {
                    window,
                    text: crate::drag::take_local_text(),
                    x,
                    y,
                }),
                3 => Some(Event::DroppedFile {
                    window,
                    path: crate::drag::take_local_text(),
                    x,
                    y,
                }),
                _ => Some(Event::DropEnded { window, x, y }),
            }
        }
        _ => None,
    }
}

fn decode_window(e: &SDL_WindowEvent) -> Option<Event> {
    let window = window_id(e.windowID);
    let (d1, d2) = (i64::from(e.data1), i64::from(e.data2));
    match e.r#type {
        SDL_EVENT_WINDOW_SHOWN => Some(Event::WindowShown { window }),
        SDL_EVENT_WINDOW_HIDDEN => Some(Event::WindowHidden { window }),
        SDL_EVENT_WINDOW_EXPOSED => Some(Event::RedrawRequested { window }),
        SDL_EVENT_WINDOW_MOVED => Some(Event::Moved {
            window,
            x: d1,
            y: d2,
        }),
        SDL_EVENT_WINDOW_RESIZED => Some(Event::Resized {
            window,
            width: d1,
            height: d2,
        }),
        SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED => Some(Event::PixelSizeChanged {
            window,
            width: d1,
            height: d2,
        }),
        SDL_EVENT_WINDOW_MINIMIZED => Some(Event::Minimized { window }),
        SDL_EVENT_WINDOW_MAXIMIZED => Some(Event::Maximized { window }),
        SDL_EVENT_WINDOW_RESTORED => Some(Event::Restored { window }),
        SDL_EVENT_WINDOW_MOUSE_ENTER => Some(Event::PointerEntered { window }),
        SDL_EVENT_WINDOW_MOUSE_LEAVE => Some(Event::PointerLeft { window }),
        SDL_EVENT_WINDOW_FOCUS_GAINED => Some(Event::FocusGained { window }),
        SDL_EVENT_WINDOW_FOCUS_LOST => Some(Event::FocusLost { window }),
        SDL_EVENT_WINDOW_CLOSE_REQUESTED => Some(Event::CloseRequested { window }),
        SDL_EVENT_WINDOW_DISPLAY_CHANGED => Some(Event::DisplayChanged {
            window,
            display: DisplayId(e.data1 as u32),
        }),
        SDL_EVENT_WINDOW_DISPLAY_SCALE_CHANGED => {
            // SAFETY: SDL answers null for a window already gone.
            let handle = unsafe { SDL_GetWindowFromID(e.windowID) };
            let scale = if handle.is_null() {
                1.0
            } else {
                // SAFETY: handle is a live window.
                f64::from(unsafe { SDL_GetWindowDisplayScale(handle) })
            };
            Some(Event::ScaleChanged { window, scale })
        }
        SDL_EVENT_WINDOW_OCCLUDED => Some(Event::Occluded { window }),
        SDL_EVENT_WINDOW_ENTER_FULLSCREEN => Some(Event::EnteredFullscreen { window }),
        SDL_EVENT_WINDOW_LEAVE_FULLSCREEN => Some(Event::LeftFullscreen { window }),
        _ => None,
    }
}

fn decode_display(e: &SDL_DisplayEvent) -> Option<Event> {
    let display = DisplayId(e.displayID.0);
    match e.r#type {
        SDL_EVENT_DISPLAY_ADDED => Some(Event::DisplayAdded { display }),
        SDL_EVENT_DISPLAY_REMOVED => Some(Event::DisplayRemoved { display }),
        SDL_EVENT_DISPLAY_MOVED => Some(Event::DisplayMoved { display }),
        SDL_EVENT_DISPLAY_DESKTOP_MODE_CHANGED | SDL_EVENT_DISPLAY_CURRENT_MODE_CHANGED => {
            Some(Event::DisplayModeChanged { display })
        }
        SDL_EVENT_DISPLAY_CONTENT_SCALE_CHANGED => Some(Event::DisplayScaleChanged { display }),
        SDL_EVENT_DISPLAY_USABLE_BOUNDS_CHANGED => {
            Some(Event::DisplayUsableBoundsChanged { display })
        }
        SDL_EVENT_DISPLAY_ORIENTATION => Some(Event::DisplayOrientationChanged {
            display,
            orientation: orientation_from(e.data1),
        }),
        _ => None,
    }
}

fn decode_key(e: &SDL_KeyboardEvent) -> Option<Event> {
    let window = window_id(e.windowID);
    let key = Key::from_code(e.key.0);
    let scancode = Scancode::from_code(e.scancode.0);
    let mods = mods_from(e.r#mod);
    if e.r#type == SDL_EVENT_KEY_DOWN {
        Some(Event::KeyDown {
            window,
            key,
            scancode,
            mods,
            repeat: e.repeat,
        })
    } else {
        Some(Event::KeyUp {
            window,
            key,
            scancode,
            mods,
        })
    }
}

fn source_from(which: u32) -> PointerSource {
    if which == SDL_TOUCH_MOUSEID.0 {
        PointerSource::Touch
    } else if which == SDL_PEN_MOUSEID.0 {
        PointerSource::Pen
    } else {
        PointerSource::Mouse
    }
}

fn button_from(raw: u8) -> MouseButton {
    match raw {
        1 => MouseButton::Left,
        2 => MouseButton::Middle,
        3 => MouseButton::Right,
        4 => MouseButton::Back,
        5 => MouseButton::Forward,
        other => MouseButton::Other(i64::from(other)),
    }
}

/// The buttons SDL's bit set `raw` holds.
pub(crate) fn buttons_from(raw: SDL_MouseButtonFlags) -> Buttons {
    Buttons {
        left: raw.0 & SDL_BUTTON_LMASK.0 != 0,
        middle: raw.0 & SDL_BUTTON_MMASK.0 != 0,
        right: raw.0 & SDL_BUTTON_RMASK.0 != 0,
        back: raw.0 & SDL_BUTTON_X1MASK.0 != 0,
        forward: raw.0 & SDL_BUTTON_X2MASK.0 != 0,
    }
}

fn decode_motion(e: &SDL_MouseMotionEvent) -> Option<Event> {
    Some(Event::PointerMoved {
        window: window_id(e.windowID),
        x: f64::from(e.x),
        y: f64::from(e.y),
        dx: f64::from(e.xrel),
        dy: f64::from(e.yrel),
        buttons: buttons_from(e.state),
        source: source_from(e.which.0),
    })
}

fn decode_button(e: &SDL_MouseButtonEvent) -> Option<Event> {
    let window = window_id(e.windowID);
    let button = button_from(e.button);
    let (x, y) = (f64::from(e.x), f64::from(e.y));
    let clicks = i64::from(e.clicks);
    let source = source_from(e.which.0);
    if e.down {
        Some(Event::ButtonDown {
            window,
            button,
            x,
            y,
            clicks,
            source,
        })
    } else {
        Some(Event::ButtonUp {
            window,
            button,
            x,
            y,
            clicks,
            source,
        })
    }
}

fn decode_wheel(e: &SDL_MouseWheelEvent) -> Option<Event> {
    // SDL reports a flipped wheel (natural scrolling) with the deltas
    // negated; undoing it keeps the event's direction physical.
    let sign = if e.direction == SDL_MOUSEWHEEL_FLIPPED {
        -1.0
    } else {
        1.0
    };
    Some(Event::Wheel {
        window: window_id(e.windowID),
        dx: f64::from(e.x) * sign,
        dy: f64::from(e.y) * sign,
        x: f64::from(e.mouse_x),
        y: f64::from(e.mouse_y),
    })
}

// A touch arrives normalized to the window; the event carries the window's
// logical coordinates, as pointer events do. The scaling stays in f32, the
// precision SDL measures every pointer coordinate in.
fn window_size(id: SDL_WindowID) -> (f32, f32) {
    // SAFETY: SDL answers null for an unknown id.
    let handle = unsafe { SDL_GetWindowFromID(id) };
    if handle.is_null() {
        return (1.0, 1.0);
    }
    let (mut w, mut h) = (0, 0);
    // SAFETY: handle is live; w and h are valid storage.
    unsafe { SDL_GetWindowSize(handle, &mut w, &mut h) };
    (w as f32, h as f32)
}

fn decode_touch(e: &SDL_TouchFingerEvent) -> Option<Event> {
    let window = window_id(e.windowID);
    let (w, h) = window_size(e.windowID);
    let x = f64::from(e.x * w);
    let y = f64::from(e.y * h);
    let (device, finger) = (e.touchID.0, e.fingerID.0);
    match e.r#type {
        SDL_EVENT_FINGER_DOWN => Some(Event::TouchDown {
            window,
            device,
            finger,
            x,
            y,
            pressure: f64::from(e.pressure),
        }),
        SDL_EVENT_FINGER_MOTION => Some(Event::TouchMoved {
            window,
            device,
            finger,
            x,
            y,
            dx: f64::from(e.dx * w),
            dy: f64::from(e.dy * h),
            pressure: f64::from(e.pressure),
        }),
        SDL_EVENT_FINGER_UP => Some(Event::TouchUp {
            window,
            device,
            finger,
            x,
            y,
        }),
        _ => Some(Event::TouchCancelled {
            window,
            device,
            finger,
        }),
    }
}

fn decode_pinch(e: &SDL_PinchFingerEvent) -> Option<Event> {
    let window = window_id(e.windowID);
    match e.r#type {
        SDL_EVENT_PINCH_BEGIN => Some(Event::PinchBegan { window }),
        SDL_EVENT_PINCH_UPDATE => Some(Event::Pinch {
            window,
            scale: f64::from(e.scale),
        }),
        _ => Some(Event::PinchEnded { window }),
    }
}

fn decode_drop(e: &SDL_DropEvent) -> Option<Event> {
    let window = window_id(e.windowID);
    let (x, y) = (f64::from(e.x), f64::from(e.y));
    match e.r#type {
        SDL_EVENT_DROP_BEGIN => Some(Event::DropBegan { window }),
        SDL_EVENT_DROP_POSITION => Some(Event::DropMoved { window, x, y }),
        // A file with no window is one the system asked the application to
        // open (macOS Finder, "Open With", a document dropped on the Dock icon).
        // SDL reports a file dropped as `text/uri-list` in URI form
        // (`/C:/x` on Windows) and one from a native file list as is.
        SDL_EVENT_DROP_FILE if e.windowID.0 == 0 => Some(Event::OpenRequested {
            path: native_path(&support::read_text(e.data)),
        }),
        SDL_EVENT_DROP_FILE => Some(Event::DroppedFile {
            window,
            path: native_path(&support::read_text(e.data)),
            x,
            y,
        }),
        SDL_EVENT_DROP_TEXT => Some(Event::DroppedText {
            window,
            text: support::read_text(e.data),
            x,
            y,
        }),
        _ => Some(Event::DropEnded { window, x, y }),
    }
}

fn pen_state_from(raw: SDL_PenInputFlags) -> PenState {
    let has = |bit: SDL_PenInputFlags| raw.0 & bit.0 != 0;
    PenState {
        down: has(SDL_PEN_INPUT_DOWN),
        eraser: has(SDL_PEN_INPUT_ERASER_TIP),
        primary_button: has(SDL_PEN_INPUT_BUTTON_1),
        secondary_button: has(SDL_PEN_INPUT_BUTTON_2),
        tertiary_button: has(SDL_PEN_INPUT_BUTTON_3),
    }
}

fn pen_axis_from(raw: i32) -> PenAxis {
    match raw {
        0 => PenAxis::Pressure,
        1 => PenAxis::TiltX,
        2 => PenAxis::TiltY,
        3 => PenAxis::Distance,
        4 => PenAxis::Rotation,
        5 => PenAxis::Slider,
        6 => PenAxis::TangentialPressure,
        other => PenAxis::Other(i64::from(other)),
    }
}
