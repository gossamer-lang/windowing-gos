//! Windows: creation, the state a program changes, text input, the pointer
//! modes a window takes, custom title-bar hit testing, and redraw requests.

use std::ffi::c_void;

use parking_lot::Mutex;
use sdl3_sys::keyboard::*;
use sdl3_sys::mouse::{
    SDL_GetWindowRelativeMouseMode, SDL_SetWindowRelativeMouseMode, SDL_WarpMouseInWindow,
};
use sdl3_sys::properties::{
    SDL_CreateProperties, SDL_DestroyProperties, SDL_GetPointerProperty, SDL_PropertiesID,
    SDL_SetBooleanProperty, SDL_SetNumberProperty, SDL_SetPointerProperty, SDL_SetStringProperty,
};
use sdl3_sys::rect::SDL_Rect;
use sdl3_sys::stdinc::SDL_free;
use sdl3_sys::surface::SDL_DestroySurface;
use sdl3_sys::video::*;

use crate::display::{self, Display, Mode, Rect};
use crate::{Error, Result, support};

/// A window, as events name it. No open window has the id 0.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct WindowId(pub u32);

/// What a window is for, which decides how the system decorates, stacks,
/// and focuses it. A popup, tooltip, or modal needs a parent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WindowKind {
    #[default]
    Normal,
    /// A tool palette: no task-bar entry, stays above its parent.
    Utility,
    /// A menu or drop-down, placed relative to its parent.
    Popup,
    /// A tooltip, placed relative to its parent, never focused.
    Tooltip,
    /// A dialog that blocks input to its parent while it is open.
    Modal,
}

/// The graphics API a window is created for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Graphics {
    /// Draw into the framebuffer `Window::framebuffer` answers.
    #[default]
    Software,
    /// An OpenGL or OpenGL ES context; see `Window::gl_context`.
    OpenGl,
    /// A Vulkan surface; see `Window::vulkan_surface`.
    Vulkan,
    /// A Metal layer (macOS); see `Window::metal_view`.
    Metal,
    /// The native handle only, for a graphics API bound by another package.
    External,
}

/// How a window asks for the user's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Attention {
    /// Flash once.
    Briefly,
    /// Flash until the window gains focus.
    UntilFocused,
    /// Stop asking.
    Cancel,
}

/// The material Windows 11 draws behind a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Backdrop {
    Auto,
    Plain,
    Mica,
    Acrylic,
    Tabbed,
}

/// What a point of a borderless window is to the system: where a custom
/// title bar drags the window or an edge resizes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HitArea {
    Normal,
    Draggable,
    ResizeTopLeft,
    ResizeTop,
    ResizeTopRight,
    ResizeRight,
    ResizeBottomRight,
    ResizeBottom,
    ResizeBottomLeft,
    ResizeLeft,
}

/// An open window. The value is a reference to it: copies name the same
/// window, and it stays open until `close` or `App::finalize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub(crate) handle: *mut SDL_Window,
    pub(crate) id: WindowId,
    pub(crate) redraw_type: u32,
}

// SAFETY: a Window is an identity; every call that dereferences the handle
// first checks it runs on the main thread, which SDL requires.
unsafe impl Send for Window {}
// SAFETY: see the Send impl; shared references only copy the identity.
unsafe impl Sync for Window {}

fn content_size(width: i64, height: i64) -> Result<(i32, i32)> {
    if width <= 0 || height <= 0 {
        return Err(Error::new(format!(
            "a window needs a positive size, not {width}x{height}"
        )));
    }
    Ok((
        support::c_int(width, "a window width")?,
        support::c_int(height, "a window height")?,
    ))
}

impl Window {
    /// The id events carry for this window.
    pub fn id(&self) -> WindowId {
        self.id
    }

    /// Closes the window. Events already queued may still name its id.
    pub fn close(&self) {
        support::require_main_thread("Window::close");
        crate::access::detach(self);
        // SAFETY: handle is a live window on the main thread.
        unsafe { SDL_DestroyWindow(self.handle) }
    }

    pub fn title(&self) -> String {
        support::require_main_thread("Window::title");
        // SAFETY: handle is a live window.
        support::read_text(unsafe { SDL_GetWindowTitle(self.handle) })
    }

    pub fn set_title(&self, title: &str) -> Result<()> {
        support::require_main_thread("Window::set_title");
        let text = support::c_text(title, "a window title")?;
        // SAFETY: handle is live; text outlives the call.
        support::check(
            unsafe { SDL_SetWindowTitle(self.handle, text.as_ptr()) },
            "setting the window title",
        )
    }

    /// The size of the window's content area in logical units.
    pub fn size(&self) -> (i64, i64) {
        support::require_main_thread("Window::size");
        let (mut w, mut h) = (0, 0);
        // SAFETY: handle is live; w and h are valid storage.
        unsafe { SDL_GetWindowSize(self.handle, &mut w, &mut h) };
        (i64::from(w), i64::from(h))
    }

    pub fn set_size(&self, width: i64, height: i64) -> Result<()> {
        support::require_main_thread("Window::set_size");
        let (w, h) = content_size(width, height)?;
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowSize(self.handle, w, h) },
            "resizing the window",
        )
    }

    /// The size of the window's content area in pixels.
    pub fn pixel_size(&self) -> (i64, i64) {
        support::require_main_thread("Window::pixel_size");
        let (mut w, mut h) = (0, 0);
        // SAFETY: handle is live; w and h are valid storage.
        unsafe { SDL_GetWindowSizeInPixels(self.handle, &mut w, &mut h) };
        (i64::from(w), i64::from(h))
    }

    /// Pixels per logical unit, including the display's content scale.
    pub fn scale(&self) -> f64 {
        support::require_main_thread("Window::scale");
        // SAFETY: handle is live.
        f64::from(unsafe { SDL_GetWindowDisplayScale(self.handle) })
    }

    pub fn position(&self) -> (i64, i64) {
        support::require_main_thread("Window::position");
        let (mut x, mut y) = (0, 0);
        // SAFETY: handle is live; x and y are valid storage.
        unsafe { SDL_GetWindowPosition(self.handle, &mut x, &mut y) };
        (i64::from(x), i64::from(y))
    }

    pub fn set_position(&self, x: i64, y: i64) -> Result<()> {
        support::require_main_thread("Window::set_position");
        let cx = support::c_int(x, "a window x position")?;
        let cy = support::c_int(y, "a window y position")?;
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowPosition(self.handle, cx, cy) },
            "moving the window",
        )
    }

    pub fn set_min_size(&self, width: i64, height: i64) -> Result<()> {
        support::require_main_thread("Window::set_min_size");
        let w = support::c_int(width, "a minimum width")?;
        let h = support::c_int(height, "a minimum height")?;
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowMinimumSize(self.handle, w, h) },
            "setting the minimum size",
        )
    }

    pub fn set_max_size(&self, width: i64, height: i64) -> Result<()> {
        support::require_main_thread("Window::set_max_size");
        let w = support::c_int(width, "a maximum width")?;
        let h = support::c_int(height, "a maximum height")?;
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowMaximumSize(self.handle, w, h) },
            "setting the maximum size",
        )
    }

    pub fn set_resizable(&self, resizable: bool) -> Result<()> {
        support::require_main_thread("Window::set_resizable");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowResizable(self.handle, resizable) },
            "setting resizability",
        )
    }

    pub fn set_bordered(&self, bordered: bool) -> Result<()> {
        support::require_main_thread("Window::set_bordered");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowBordered(self.handle, bordered) },
            "setting the border",
        )
    }

    /// Enters or leaves desktop fullscreen.
    pub fn set_fullscreen(&self, fullscreen: bool) -> Result<()> {
        support::require_main_thread("Window::set_fullscreen");
        support::check(
            // SAFETY: handle is live; null selects desktop fullscreen.
            unsafe { SDL_SetWindowFullscreenMode(self.handle, std::ptr::null()) },
            "choosing desktop fullscreen",
        )?;
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowFullscreen(self.handle, fullscreen) },
            "changing fullscreen",
        )
    }

    /// Enters exclusive fullscreen in `mode`, one of `display::modes`.
    pub fn set_fullscreen_mode(&self, mode: Mode) -> Result<()> {
        support::require_main_thread("Window::set_fullscreen_mode");
        // SAFETY: handle is live.
        let display = unsafe { SDL_GetDisplayForWindow(self.handle) };
        let mut count = 0;
        // SAFETY: count receives the list length.
        let list = unsafe { SDL_GetFullscreenDisplayModes(display, &mut count) };
        if list.is_null() {
            return Err(support::failure("listing display modes"));
        }
        // SAFETY: SDL answered `count` mode pointers.
        let entries = unsafe { std::slice::from_raw_parts(list, count as usize) };
        let chosen = entries
            .iter()
            .copied()
            // SAFETY: each non-null entry is a mode SDL owns until the free.
            .find(|p| !p.is_null() && display::mode_from(unsafe { &**p }) == mode);
        // SDL copies the mode it is given, so the list may go once it is set.
        let result = match chosen {
            Some(candidate) => support::check(
                // SAFETY: candidate is a live entry of the list.
                unsafe { SDL_SetWindowFullscreenMode(self.handle, candidate) },
                "choosing the fullscreen mode",
            ),
            None => Err(Error::new(format!(
                "the display has no mode {}x{} at {} Hz",
                mode.width, mode.height, mode.refresh_rate
            ))),
        };
        // SAFETY: the list was allocated for the caller to free.
        unsafe { SDL_free(list.cast::<c_void>()) };
        result?;
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowFullscreen(self.handle, true) },
            "entering fullscreen",
        )
    }

    pub fn is_fullscreen(&self) -> bool {
        self.flag(SDL_WINDOW_FULLSCREEN)
    }

    pub fn is_maximized(&self) -> bool {
        self.flag(SDL_WINDOW_MAXIMIZED)
    }

    pub fn is_minimized(&self) -> bool {
        self.flag(SDL_WINDOW_MINIMIZED)
    }

    pub fn is_visible(&self) -> bool {
        !self.flag(SDL_WINDOW_HIDDEN)
    }

    pub fn has_focus(&self) -> bool {
        self.flag(SDL_WINDOW_INPUT_FOCUS)
    }

    pub fn maximize(&self) -> Result<()> {
        support::require_main_thread("Window::maximize");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_MaximizeWindow(self.handle) },
            "maximizing the window",
        )
    }

    pub fn minimize(&self) -> Result<()> {
        support::require_main_thread("Window::minimize");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_MinimizeWindow(self.handle) },
            "minimizing the window",
        )
    }

    pub fn restore(&self) -> Result<()> {
        support::require_main_thread("Window::restore");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_RestoreWindow(self.handle) },
            "restoring the window",
        )
    }

    pub fn set_always_on_top(&self, on_top: bool) -> Result<()> {
        support::require_main_thread("Window::set_always_on_top");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowAlwaysOnTop(self.handle, on_top) },
            "setting always-on-top",
        )
    }

    pub fn opacity(&self) -> f64 {
        support::require_main_thread("Window::opacity");
        // SAFETY: handle is live.
        f64::from(unsafe { SDL_GetWindowOpacity(self.handle) })
    }

    pub fn set_opacity(&self, opacity: f64) -> Result<()> {
        support::require_main_thread("Window::set_opacity");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowOpacity(self.handle, opacity as f32) },
            "setting the opacity",
        )
    }

    pub fn show(&self) -> Result<()> {
        support::require_main_thread("Window::show");
        // SAFETY: handle is live.
        support::check(unsafe { SDL_ShowWindow(self.handle) }, "showing the window")
    }

    pub fn hide(&self) -> Result<()> {
        support::require_main_thread("Window::hide");
        // SAFETY: handle is live.
        support::check(unsafe { SDL_HideWindow(self.handle) }, "hiding the window")
    }

    /// Raises the window and asks for input focus.
    pub fn focus(&self) -> Result<()> {
        support::require_main_thread("Window::focus");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_RaiseWindow(self.handle) },
            "raising the window",
        )
    }

    /// Obtains a single-use Wayland activation token for this surface, with a
    /// two-second deadline. Other video drivers answer an empty token.
    pub fn activation_token(&self) -> Result<String> {
        support::require_main_thread("Window::activation_token");
        #[cfg(target_os = "linux")]
        if crate::video_driver() == "wayland" {
            return crate::drag::activation_token(self);
        }
        Ok(String::new())
    }

    /// Preserves the desktop's per-launch focus context on X11 and Wayland.
    pub fn focus_with_context(&self, activation: &crate::instance::Activation) -> Result<()> {
        activation.validate()?;
        self.focus_with_activation(&activation.token)?;
        #[cfg(target_os = "linux")]
        if !activation.startup_id.is_empty()
            && let crate::surface::RawHandle::Xlib {
                display,
                window,
                screen,
            } = self.raw_handle()?
        {
            return crate::xlib::activate(display, window, screen, &activation.startup_id);
        }
        Ok(())
    }

    /// Requests focus using the desktop's single-use Wayland launch token.
    /// An empty token uses the ordinary platform focus request. Compositor
    /// policy still decides whether focus may move.
    pub fn focus_with_activation(&self, token: &str) -> Result<()> {
        support::require_main_thread("Window::focus_with_activation");
        if token.len() > 4096 || token.contains('\0') {
            return Err(Error::new("invalid desktop activation token"));
        }
        #[cfg(target_os = "linux")]
        if !token.is_empty() && crate::video_driver() == "wayland" {
            return crate::drag::activate(self, token);
        }
        self.focus()
    }

    pub fn request_attention(&self, how: Attention) -> Result<()> {
        support::require_main_thread("Window::request_attention");
        let op = match how {
            Attention::Briefly => SDL_FLASH_BRIEFLY,
            Attention::UntilFocused => SDL_FLASH_UNTIL_FOCUSED,
            Attention::Cancel => SDL_FLASH_CANCEL,
        };
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_FlashWindow(self.handle, op) },
            "requesting attention",
        )
    }

    pub fn set_parent(&self, parent: Option<Window>) -> Result<()> {
        support::require_main_thread("Window::set_parent");
        let handle = parent.map_or(std::ptr::null_mut(), |p| p.handle);
        // SAFETY: both handles are live or the parent is null.
        support::check(
            unsafe { SDL_SetWindowParent(self.handle, handle) },
            "setting the parent window",
        )
    }

    /// The display the window is on.
    pub fn display(&self) -> Result<Display> {
        support::require_main_thread("Window::display");
        // SAFETY: handle is live.
        let id = unsafe { SDL_GetDisplayForWindow(self.handle) };
        if id.0 == 0 {
            return Err(support::failure("finding the window's display"));
        }
        display::get(display::DisplayId(id.0))
    }

    /// Queues `Event::RedrawRequested` for this window; safe from any thread.
    pub fn request_redraw(&self) -> Result<()> {
        crate::waker::push_user_event(self.redraw_type, self.id.0, 0, 0, 0)
    }

    pub fn start_text_input(&self) -> Result<()> {
        support::require_main_thread("Window::start_text_input");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_StartTextInput(self.handle) },
            "starting text input",
        )
    }

    /// Starts text input, describing the text to the input method.
    pub fn start_text_input_with(&self, options: &TextInputOptions) -> Result<()> {
        support::require_main_thread("Window::start_text_input_with");
        let props = Properties::new("creating text input properties")?;
        props.set_number(SDL_PROP_TEXTINPUT_TYPE_NUMBER, options.purpose.sdl_type())?;
        if let Some(capitalization) = options.capitalization {
            props.set_number(
                SDL_PROP_TEXTINPUT_CAPITALIZATION_NUMBER,
                capitalization.sdl_value(),
            )?;
        }
        props.set_bool(SDL_PROP_TEXTINPUT_AUTOCORRECT_BOOLEAN, options.autocorrect)?;
        props.set_bool(SDL_PROP_TEXTINPUT_MULTILINE_BOOLEAN, options.multiline)?;
        support::check(
            // SAFETY: handle is live; props lives until the end of scope.
            unsafe { SDL_StartTextInputWithProperties(self.handle, props.0) },
            "starting text input",
        )
    }

    pub fn stop_text_input(&self) -> Result<()> {
        support::require_main_thread("Window::stop_text_input");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_StopTextInput(self.handle) },
            "stopping text input",
        )
    }

    pub fn is_text_input_active(&self) -> bool {
        support::require_main_thread("Window::is_text_input_active");
        // SAFETY: handle is live.
        unsafe { SDL_TextInputActive(self.handle) }
    }

    /// Where the text being edited is, so the input method places its
    /// candidate window beside it; `cursor` is the caret's x offset in `area`.
    pub fn set_text_input_area(&self, area: Rect, cursor: i64) -> Result<()> {
        support::require_main_thread("Window::set_text_input_area");
        let rect = SDL_Rect {
            x: support::c_int(area.x, "a text area x")?,
            y: support::c_int(area.y, "a text area y")?,
            w: support::c_int(area.width, "a text area width")?,
            h: support::c_int(area.height, "a text area height")?,
        };
        let caret = support::c_int(cursor, "a text cursor offset")?;
        support::check(
            // SAFETY: handle is live; rect is valid for the call.
            unsafe { SDL_SetTextInputArea(self.handle, &rect, caret) },
            "placing the text input area",
        )
    }

    /// Relative mode hides the pointer, keeps it in the window, and reports
    /// unbounded motion in `PointerMoved`'s deltas.
    pub fn set_relative_pointer(&self, enabled: bool) -> Result<()> {
        support::require_main_thread("Window::set_relative_pointer");
        support::check(
            // SAFETY: handle is live.
            unsafe { SDL_SetWindowRelativeMouseMode(self.handle, enabled) },
            "changing relative pointer mode",
        )
    }

    pub fn is_relative_pointer(&self) -> bool {
        support::require_main_thread("Window::is_relative_pointer");
        // SAFETY: handle is live.
        unsafe { SDL_GetWindowRelativeMouseMode(self.handle) }
    }

    /// Keeps the pointer inside the window while it has focus.
    pub fn set_pointer_confined(&self, confined: bool) -> Result<()> {
        support::require_main_thread("Window::set_pointer_confined");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SetWindowMouseGrab(self.handle, confined) },
            "confining the pointer",
        )
    }

    /// Moves the pointer to `x`, `y` in the window's logical coordinates.
    pub fn warp_pointer(&self, x: f64, y: f64) {
        support::require_main_thread("Window::warp_pointer");
        // SAFETY: handle is live.
        unsafe { SDL_WarpMouseInWindow(self.handle, x as f32, y as f32) }
    }

    /// Makes `area` decide what each point of the window is to the system,
    /// so a borderless window with a drawn title bar drags and resizes like
    /// a decorated one; `None` restores the default.
    pub fn set_hit_test(
        &self,
        area: Option<Box<dyn Fn(f64, f64) -> HitArea + Send>>,
    ) -> Result<()> {
        support::require_main_thread("Window::set_hit_test");
        crate::hooks::set_hit_test(self.handle, area)
    }

    /// The window's icon, from `width` x `height` RGBA pixels, 4 bytes each.
    pub fn set_icon(&self, rgba: &[u8], width: i64, height: i64) -> Result<()> {
        support::require_main_thread("Window::set_icon");
        let surface = crate::surface::rgba_surface(rgba, width, height)?;
        // SAFETY: handle and surface are live; SDL copies the icon.
        let ok = unsafe { SDL_SetWindowIcon(self.handle, surface) };
        // SAFETY: surface was created for this call.
        unsafe { SDL_DestroySurface(surface) };
        support::check(ok, "setting the window icon")
    }

    /// The material Windows 11 draws behind the window. Elsewhere, and
    /// before Windows 11 22H2, this answers an error.
    pub fn set_backdrop(&self, backdrop: Backdrop) -> Result<()> {
        support::require_main_thread("Window::set_backdrop");
        let kind = match backdrop {
            Backdrop::Auto => 0,
            Backdrop::Plain => 1,
            Backdrop::Mica => 2,
            Backdrop::Acrylic => 3,
            Backdrop::Tabbed => 4,
        };
        let hwnd = self.pointer_property(SDL_PROP_WINDOW_WIN32_HWND_POINTER);
        if hwnd.is_null() {
            return Err(Error::new("a window backdrop is a Windows feature"));
        }
        crate::win32::set_backdrop(hwnd, kind)
    }

    /// Waits until the platform has applied the window's pending position,
    /// size, and state changes.
    pub fn sync(&self) -> Result<()> {
        support::require_main_thread("Window::sync");
        // SAFETY: handle is live.
        support::check(
            unsafe { SDL_SyncWindow(self.handle) },
            "waiting for the window to settle",
        )
    }

    /// Opts this window into native macOS tabs with a stable group identifier.
    pub fn set_native_tabbing(&self, group: &str, enabled: bool) -> Result<()> {
        support::require_main_thread("Window::set_native_tabbing");
        if enabled && group.is_empty() {
            return Err(Error::new("a native tab group needs an identifier"));
        }
        crate::macos::configure_tabs(self.appkit_window()?, group, enabled)
    }

    /// Groups `other` with this window using the platform's native tab strip.
    pub fn merge_native_tab(&self, other: Window) -> Result<()> {
        support::require_main_thread("Window::merge_native_tab");
        if self.id == other.id {
            return Err(Error::new("cannot merge a window with itself"));
        }
        crate::macos::merge_tabs(self.appkit_window()?, other.appkit_window()?)
    }

    /// Detaches this native tab into its own window without destroying its content.
    pub fn detach_native_tab(&self) -> Result<()> {
        support::require_main_thread("Window::detach_native_tab");
        crate::macos::detach_tab(self.appkit_window()?)
    }

    fn appkit_window(&self) -> Result<*mut c_void> {
        match self.raw_handle()? {
            crate::surface::RawHandle::AppKit { ns_window, .. } => Ok(ns_window),
            _ => Err(Error::new("native window tabs require macOS AppKit")),
        }
    }

    pub(crate) fn pointer_property(&self, name: *const std::ffi::c_char) -> *mut c_void {
        // SAFETY: handle is live; SDL answers null for an unset property.
        unsafe {
            SDL_GetPointerProperty(
                SDL_GetWindowProperties(self.handle),
                name,
                std::ptr::null_mut(),
            )
        }
    }

    fn flag(&self, flag: SDL_WindowFlags) -> bool {
        support::require_main_thread("Window flags");
        // SAFETY: handle is live.
        unsafe { SDL_GetWindowFlags(self.handle) }.0 & flag.0 != 0
    }
}

/// Everything a window can be created with. `App::window` covers the
/// common case; `App::window_with` takes these.
#[derive(Debug, Clone)]
pub struct WindowOptions {
    pub title: String,
    /// The content area's logical size.
    pub width: i64,
    pub height: i64,
    /// The window's position in logical desktop coordinates, or relative to
    /// `parent` for a popup or tooltip; `None` lets the system place it.
    pub x: Option<i64>,
    pub y: Option<i64>,
    pub resizable: bool,
    pub borderless: bool,
    /// A content area with an alpha channel the desktop composites.
    pub transparent: bool,
    pub hidden: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub always_on_top: bool,
    /// Pixels at the display's full density rather than one per logical unit.
    pub high_density: bool,
    pub focusable: bool,
    pub graphics: Graphics,
    pub kind: WindowKind,
    /// The window this one belongs to: required for popups, tooltips, and modals.
    pub parent: Option<Window>,
}

impl Default for WindowOptions {
    fn default() -> WindowOptions {
        WindowOptions {
            title: String::new(),
            width: 800,
            height: 600,
            x: None,
            y: None,
            resizable: false,
            borderless: false,
            transparent: false,
            hidden: false,
            maximized: false,
            fullscreen: false,
            always_on_top: false,
            high_density: true,
            focusable: true,
            graphics: Graphics::Software,
            kind: WindowKind::Normal,
            parent: None,
        }
    }
}

/// What edited text is for, so an input method can pick a keyboard, hide the
/// text, or leave composition off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextPurpose {
    #[default]
    Text,
    Name,
    Email,
    Username,
    Password,
    VisiblePassword,
    Number,
    Pin,
    VisiblePin,
}

impl TextPurpose {
    // SDL_TextInputType, in declaration order.
    fn sdl_type(self) -> i64 {
        self as i64
    }
}

/// How an input method capitalizes what is typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Capitalization {
    Nothing,
    Sentences,
    Words,
    Letters,
}

impl Capitalization {
    // SDL_Capitalization, in declaration order.
    fn sdl_value(self) -> i64 {
        self as i64
    }
}

/// How `Window::start_text_input_with` describes the text to the input
/// method. `capitalization: None` takes the platform's choice for the purpose.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextInputOptions {
    pub purpose: TextPurpose,
    pub capitalization: Option<Capitalization>,
    pub autocorrect: bool,
    pub multiline: bool,
}

impl Default for TextInputOptions {
    fn default() -> TextInputOptions {
        TextInputOptions {
            purpose: TextPurpose::Text,
            capitalization: None,
            autocorrect: true,
            multiline: true,
        }
    }
}

/// What the platform's input method integration supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextInputCapabilities {
    /// `set_text_input_area` places the candidate window at the caret.
    pub cursor_area: bool,
    /// The input method can read the text around the caret. SDL 3.4 has no
    /// surrounding-text API, so this is false on every platform.
    pub surrounding_text: bool,
    /// `start_text_input_with` passes the text's purpose and hints on.
    pub purpose_hints: bool,
}

/// What text input supports here; a widget offers the features this reports.
pub fn text_input_capabilities() -> TextInputCapabilities {
    TextInputCapabilities {
        cursor_area: true,
        surrounding_text: false,
        purpose_hints: true,
    }
}

/// An SDL property set destroyed when dropped.
pub(crate) struct Properties(pub(crate) SDL_PropertiesID);

impl Properties {
    pub(crate) fn new(op: &str) -> Result<Properties> {
        // SAFETY: plain allocation.
        let id = unsafe { SDL_CreateProperties() };
        if id.0 == 0 {
            return Err(support::failure(op));
        }
        Ok(Properties(id))
    }

    pub(crate) fn set_bool(&self, name: *const std::ffi::c_char, value: bool) -> Result<()> {
        // SAFETY: name is one of SDL's NUL-terminated property names.
        support::check(
            unsafe { SDL_SetBooleanProperty(self.0, name, value) },
            "setting a property",
        )
    }

    pub(crate) fn set_number(&self, name: *const std::ffi::c_char, value: i64) -> Result<()> {
        // SAFETY: name is one of SDL's NUL-terminated property names.
        support::check(
            unsafe { SDL_SetNumberProperty(self.0, name, value) },
            "setting a property",
        )
    }

    pub(crate) fn set_string(&self, name: *const std::ffi::c_char, value: &str) -> Result<()> {
        let text = support::c_text(value, "a property value")?;
        // SAFETY: SDL copies the string.
        support::check(
            unsafe { SDL_SetStringProperty(self.0, name, text.as_ptr()) },
            "setting a property",
        )
    }

    pub(crate) fn set_pointer(
        &self,
        name: *const std::ffi::c_char,
        value: *mut c_void,
    ) -> Result<()> {
        // SAFETY: SDL stores the pointer as given.
        support::check(
            unsafe { SDL_SetPointerProperty(self.0, name, value) },
            "setting a property",
        )
    }
}

impl Drop for Properties {
    fn drop(&mut self) {
        // SAFETY: the set was created by `new` and is dropped once.
        unsafe { SDL_DestroyProperties(self.0) }
    }
}

/// Opens the window `options` describe.
pub(crate) fn create(options: &WindowOptions, redraw_type: u32) -> Result<Window> {
    let (w, h) = content_size(options.width, options.height)?;
    let kind = options.kind;
    if matches!(
        kind,
        WindowKind::Popup | WindowKind::Tooltip | WindowKind::Modal
    ) && options.parent.is_none()
    {
        return Err(Error::new(format!("a {kind:?} window needs a parent")));
    }
    let props = Properties::new("creating window properties")?;
    props.set_string(SDL_PROP_WINDOW_CREATE_TITLE_STRING, &options.title)?;
    props.set_number(SDL_PROP_WINDOW_CREATE_WIDTH_NUMBER, i64::from(w))?;
    props.set_number(SDL_PROP_WINDOW_CREATE_HEIGHT_NUMBER, i64::from(h))?;
    if let Some(px) = options.x {
        props.set_number(
            SDL_PROP_WINDOW_CREATE_X_NUMBER,
            i64::from(support::c_int(px, "a window x position")?),
        )?;
    }
    if let Some(py) = options.y {
        props.set_number(
            SDL_PROP_WINDOW_CREATE_Y_NUMBER,
            i64::from(support::c_int(py, "a window y position")?),
        )?;
    }
    props.set_bool(SDL_PROP_WINDOW_CREATE_RESIZABLE_BOOLEAN, options.resizable)?;
    props.set_bool(
        SDL_PROP_WINDOW_CREATE_BORDERLESS_BOOLEAN,
        options.borderless,
    )?;
    props.set_bool(
        SDL_PROP_WINDOW_CREATE_TRANSPARENT_BOOLEAN,
        options.transparent,
    )?;
    props.set_bool(SDL_PROP_WINDOW_CREATE_HIDDEN_BOOLEAN, options.hidden)?;
    props.set_bool(SDL_PROP_WINDOW_CREATE_MAXIMIZED_BOOLEAN, options.maximized)?;
    props.set_bool(
        SDL_PROP_WINDOW_CREATE_FULLSCREEN_BOOLEAN,
        options.fullscreen,
    )?;
    props.set_bool(
        SDL_PROP_WINDOW_CREATE_ALWAYS_ON_TOP_BOOLEAN,
        options.always_on_top,
    )?;
    props.set_bool(
        SDL_PROP_WINDOW_CREATE_HIGH_PIXEL_DENSITY_BOOLEAN,
        options.high_density,
    )?;
    props.set_bool(SDL_PROP_WINDOW_CREATE_FOCUSABLE_BOOLEAN, options.focusable)?;
    match options.graphics {
        Graphics::OpenGl => props.set_bool(SDL_PROP_WINDOW_CREATE_OPENGL_BOOLEAN, true)?,
        Graphics::Vulkan => props.set_bool(SDL_PROP_WINDOW_CREATE_VULKAN_BOOLEAN, true)?,
        Graphics::Metal => props.set_bool(SDL_PROP_WINDOW_CREATE_METAL_BOOLEAN, true)?,
        Graphics::Software | Graphics::External => {}
    }
    // SDL gives a macOS window a graphics API unless the window names one or
    // its own context; the offscreen driver's OpenGL needs EGL, which macOS
    // lacks, and a software window draws into the driver's framebuffer.
    if options.graphics == Graphics::Software && crate::video_driver() == "offscreen" {
        props.set_bool(
            SDL_PROP_WINDOW_CREATE_EXTERNAL_GRAPHICS_CONTEXT_BOOLEAN,
            true,
        )?;
    }
    match kind {
        WindowKind::Normal => {}
        WindowKind::Utility => props.set_bool(SDL_PROP_WINDOW_CREATE_UTILITY_BOOLEAN, true)?,
        WindowKind::Popup => props.set_bool(SDL_PROP_WINDOW_CREATE_MENU_BOOLEAN, true)?,
        WindowKind::Tooltip => props.set_bool(SDL_PROP_WINDOW_CREATE_TOOLTIP_BOOLEAN, true)?,
        WindowKind::Modal => props.set_bool(SDL_PROP_WINDOW_CREATE_MODAL_BOOLEAN, true)?,
    }
    if let Some(p) = options.parent {
        props.set_pointer(
            SDL_PROP_WINDOW_CREATE_PARENT_POINTER,
            p.handle.cast::<c_void>(),
        )?;
    }
    // SAFETY: props is a live property set.
    let handle = unsafe { SDL_CreateWindowWithProperties(props.0) };
    if handle.is_null() {
        return Err(support::failure("creating a window"));
    }
    if let Some(icon) = APP_ICON.lock().as_ref()
        && let Err(e) = offer_icon(handle, icon)
    {
        // SAFETY: the window was just created and is not shared.
        unsafe { SDL_DestroyWindow(handle) };
        return Err(e);
    }
    // SAFETY: handle is live.
    let id = unsafe { SDL_GetWindowID(handle) };
    let window = Window {
        handle,
        id: WindowId(id.0),
        redraw_type,
    };
    crate::access::attach(&window);
    Ok(window)
}

/// An icon's pixels, kept for the windows still to open.
struct IconPixels {
    rgba: Vec<u8>,
    width: i64,
    height: i64,
}

/// The icon `App::set_icon` gave the application.
static APP_ICON: Mutex<Option<IconPixels>> = Mutex::new(None);

/// Makes `width` x `height` RGBA pixels the icon of every open window and of
/// each window opened after.
pub(crate) fn set_app_icon(rgba: &[u8], width: i64, height: i64) -> Result<()> {
    let icon = IconPixels {
        rgba: rgba.to_vec(),
        width,
        height,
    };
    // Checked before it is kept, so a malformed image fails here rather than
    // at the next window.
    let surface = crate::surface::rgba_surface(&icon.rgba, icon.width, icon.height)?;
    // SAFETY: surface was created for this check.
    unsafe { SDL_DestroySurface(surface) };
    let mut slot = APP_ICON.lock();
    *slot = Some(icon);
    let icon = slot.as_ref().expect("the icon was stored above");
    for handle in all_windows() {
        offer_icon(handle, icon)?;
    }
    Ok(())
}

/// Every open window's handle.
pub(crate) fn all_windows() -> Vec<*mut SDL_Window> {
    let mut count = 0;
    // SAFETY: count receives the list length.
    let list = unsafe { SDL_GetWindows(&mut count) };
    if list.is_null() {
        return Vec::new();
    }
    // SAFETY: SDL answered `count` window pointers.
    let out = unsafe { std::slice::from_raw_parts(list, count as usize) }.to_vec();
    // SAFETY: the list was allocated for the caller to free.
    unsafe { SDL_free(list.cast::<c_void>()) };
    out
}

/// Gives the window `handle` the application's icon. A Wayland compositor
/// without the toplevel icon protocol, and a driver with no screen, show no
/// icon per window: there a window that takes none is the platform's answer.
fn offer_icon(handle: *mut SDL_Window, icon: &IconPixels) -> Result<()> {
    let surface = crate::surface::rgba_surface(&icon.rgba, icon.width, icon.height)?;
    // SAFETY: handle and surface are live; SDL copies the icon.
    let shown = unsafe { SDL_SetWindowIcon(handle, surface) };
    // SAFETY: surface was created for this call.
    unsafe { SDL_DestroySurface(surface) };
    let driver = crate::video_driver();
    if !shown && matches!(driver.as_str(), "wayland" | "offscreen" | "dummy") {
        return Ok(());
    }
    support::check(shown, "setting the window icon")
}

/// The window an id names while it is open.
pub(crate) fn from_id(id: WindowId, redraw_type: u32) -> Option<Window> {
    // SAFETY: SDL answers null for an unknown id.
    let handle = unsafe { SDL_GetWindowFromID(SDL_WindowID(id.0)) };
    (!handle.is_null()).then_some(Window {
        handle,
        id,
        redraw_type,
    })
}

/// The native handle each open window keeps in the pointer property `name`;
/// windows without one are left out.
#[cfg(target_os = "windows")]
pub(crate) fn native_handles(name: *const std::ffi::c_char) -> Vec<*mut c_void> {
    all_windows()
        .into_iter()
        // SAFETY: each handle is an open window; SDL answers null when unset.
        .map(|h| unsafe {
            SDL_GetPointerProperty(SDL_GetWindowProperties(h), name, std::ptr::null_mut())
        })
        .filter(|p| !p.is_null())
        .collect()
}

/// The SDL identity of the open window whose `NSWindow` is `ns_window`.
#[cfg(target_os = "macos")]
pub(crate) fn sdl_id_of_cocoa_window(ns_window: *mut c_void) -> Option<u32> {
    all_windows().into_iter().find_map(|h| {
        // SAFETY: h is an open window.
        let native = unsafe {
            SDL_GetPointerProperty(
                SDL_GetWindowProperties(h),
                SDL_PROP_WINDOW_COCOA_WINDOW_POINTER,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: h is an open window.
        (native == ns_window).then(|| unsafe { SDL_GetWindowID(h) }.0)
    })
}

/// SDL's value for a hit area.
pub(crate) fn hit_area_code(area: HitArea) -> i32 {
    area as i32
}
