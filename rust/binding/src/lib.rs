//! The `windowing_app`, `windowing_window`, and `windowing_desktop` Gossamer
//! modules: gos-windowing over the binding boundary. Windows, cursors, trays, GL contexts, and instance listeners
//! cross as ids; events cross as flat words (`wire`), and other structured
//! values as JSON in the shape the Gossamer types encode to.

mod hooks;
mod state;
mod wire;

use gos_windowing::window::WindowId;
use gos_windowing::{access, clipboard, cursor, dialog, display, instance, menu, system, tray};
use gossamer_binding::{Bytes, GosError, gos_module};
use serde::{Deserialize, Serialize};

pub use state::with_app;

fn fail(e: impl std::fmt::Display) -> GosError {
    GosError::from(e.to_string())
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn from_json<'a, T: Deserialize<'a>>(text: &'a str, what: &str) -> Result<T, GosError> {
    serde_json::from_str(text).map_err(|e| GosError::from(format!("decoding {what}: {e}")))
}

fn window(id: i64) -> Result<gos_windowing::Window, GosError> {
    with_app(|app| {
        app.window_by_id(WindowId(id as u32))
            .ok_or_else(|| GosError::from(format!("no open window {id}")))
    })
}

/// A window reference as the Gossamer `Window` struct encodes it.
#[derive(Deserialize)]
struct WindowRef {
    id: WindowId,
}

/// `WindowOptions` as Gossamer encodes it: the parent by id.
#[derive(Deserialize)]
struct WindowOptionsWire {
    title: String,
    width: i64,
    height: i64,
    x: Option<i64>,
    y: Option<i64>,
    resizable: bool,
    borderless: bool,
    transparent: bool,
    hidden: bool,
    maximized: bool,
    fullscreen: bool,
    always_on_top: bool,
    high_density: bool,
    focusable: bool,
    graphics: gos_windowing::window::Graphics,
    kind: gos_windowing::window::WindowKind,
    parent: Option<WindowRef>,
}

#[derive(Deserialize)]
struct FileDialogWire {
    title: String,
    location: String,
    filters: Vec<gos_windowing::dialog::Filter>,
    many: bool,
    parent: Option<WindowRef>,
}

#[derive(Deserialize)]
struct QuestionWire {
    title: String,
    message: String,
    buttons: Vec<String>,
    kind: gos_windowing::dialog::MessageKind,
    parent: Option<WindowRef>,
    default_button: Option<i64>,
    cancel_button: Option<i64>,
}

#[derive(Serialize)]
struct FrameInfo {
    width: i64,
    height: i64,
    stride: i64,
    format: gos_windowing::surface::PixelFormat,
}

#[derive(Serialize)]
enum RawHandleWire {
    Win32 {
        hwnd: u64,
        hinstance: u64,
    },
    AppKit {
        ns_window: u64,
        ns_view: u64,
    },
    Wayland {
        display: u64,
        surface: u64,
    },
    Xlib {
        display: u64,
        window: u64,
        screen: i64,
    },
    Drm {
        fd: i64,
        gbm_device: u64,
    },
}

/// `data` cut into consecutive pieces of `lengths`.
fn split(data: &[u8], lengths: &[i64]) -> Result<Vec<Vec<u8>>, GosError> {
    let mut at = 0usize;
    let mut out = Vec::with_capacity(lengths.len());
    for len in lengths {
        let end = at + usize::try_from(*len).map_err(|_| GosError::from("a negative length"))?;
        out.push(
            data.get(at..end)
                .ok_or_else(|| GosError::from("lengths run past the data"))?
                .to_vec(),
        );
        at = end;
    }
    Ok(out)
}

fn resolve_parent(parent: Option<WindowRef>) -> Result<Option<gos_windowing::Window>, GosError> {
    parent.map(|p| window(i64::from(p.id.0))).transpose()
}

#[gos_module("windowing_app")]
mod app_bindings {
    use super::*;

    /// Starts windowing on the calling (main) thread.
    pub fn app_init(
        name: String,
        app_id: String,
        version: String,
        driver: String,
    ) -> Result<(), GosError> {
        let app = gos_windowing::init(
            &name,
            &gos_windowing::InitOptions {
                app_id,
                version,
                driver,
            },
        )
        .map_err(fail)?;
        state::install(app);
        Ok(())
    }

    /// Ends the session and closes every window.
    pub fn app_finalize() {
        state::finalize();
    }

    /// Opens a window from encoded `WindowOptions`; answers its id.
    pub fn app_window(options: String) -> Result<i64, GosError> {
        let wire: WindowOptionsWire = from_json(&options, "window options")?;
        let opts = gos_windowing::WindowOptions {
            title: wire.title,
            width: wire.width,
            height: wire.height,
            x: wire.x,
            y: wire.y,
            resizable: wire.resizable,
            borderless: wire.borderless,
            transparent: wire.transparent,
            hidden: wire.hidden,
            maximized: wire.maximized,
            fullscreen: wire.fullscreen,
            always_on_top: wire.always_on_top,
            high_density: wire.high_density,
            focusable: wire.focusable,
            graphics: wire.graphics,
            kind: wire.kind,
            parent: resolve_parent(wire.parent)?,
        };
        with_app(|app| {
            app.window_with(&opts)
                .map(|w| i64::from(w.id().0))
                .map_err(fail)
        })
    }

    /// Whether window `id` is open.
    pub fn app_window_exists(id: i64) -> bool {
        window(id).is_ok()
    }

    pub fn app_set_icon(rgba: Bytes, width: i64, height: i64) -> Result<(), GosError> {
        with_app(|app| app.set_icon(&rgba.0, width, height).map_err(fail))
    }

    /// Wakes `app_wait`; safe from any thread.
    pub fn app_wake() -> Result<(), GosError> {
        state::waker()
            .ok_or_else(|| GosError::from("windowing is not running"))?
            .wake()
            .map_err(fail)
    }

    pub fn app_mods() -> Result<String, GosError> {
        with_app(|app| Ok(to_json(&app.mods())))
    }

    pub fn app_is_key_down(scancode: i64) -> Result<bool, GosError> {
        with_app(|app| Ok(app.is_key_down(gos_windowing::Scancode::from_code(scancode as i32))))
    }

    pub fn app_key_at(scancode: i64) -> Result<i64, GosError> {
        with_app(|app| {
            Ok(i64::from(
                app.key_at(gos_windowing::Scancode::from_code(scancode as i32))
                    .code(),
            ))
        })
    }

    pub fn app_key_name(keycode: i64) -> Result<String, GosError> {
        with_app(|app| Ok(app.key_name(gos_windowing::Key::from_code(keycode as u32))))
    }

    /// The pointer's position and held buttons, encoded as `[x, y, buttons]`.
    pub fn app_pointer() -> Result<String, GosError> {
        with_app(|app| Ok(to_json(&app.pointer())))
    }

    pub fn app_capture_pointer(captured: bool) -> Result<(), GosError> {
        with_app(|app| app.capture_pointer(captured).map_err(fail))
    }

    /// Queues an event, as its words, as if the platform had delivered it;
    /// an event with texts reads those `simulate_texts` kept. The words
    /// cross as scalars, which need no allocation on either side.
    pub fn app_simulate(
        tag: i64,
        w1: i64,
        w2: i64,
        w3: i64,
        w4: i64,
        w5: i64,
        w6: i64,
        w7: i64,
        w8: i64,
    ) -> Result<(), GosError> {
        let texts = if wire::carries_texts(tag) {
            wire::take_texts()
        } else {
            Vec::new()
        };
        let e = wire::decode(&[tag, w1, w2, w3, w4, w5, w6, w7, w8], texts).map_err(fail)?;
        state::with_event_app(|app| app.simulate(e).map_err(fail))
    }

    /// Keeps the texts of the event the next `app_simulate` queues.
    pub fn simulate_texts(texts: Vec<String>) {
        wire::keep_texts(texts)
    }

    /// The tag of the event `json` encodes, its words and texts kept as a
    /// pump keeps them, so a test can hold the two sides' layouts against
    /// each other.
    pub fn event_wire(json: String) -> Result<i64, GosError> {
        let e: gos_windowing::Event = from_json(&json, "an event")?;
        Ok(wire::answer(&e))
    }

    /// The JSON of the event the words and texts name.
    pub fn event_json(words: Vec<i64>, texts: Vec<String>) -> Result<String, GosError> {
        wire::decode(&words, texts)
            .map(|e| to_json(&e))
            .map_err(fail)
    }

    pub fn video_driver() -> String {
        gos_windowing::video_driver()
    }

    pub fn capabilities() -> String {
        to_json(&gos_windowing::capabilities())
    }

    pub fn sdl_version() -> String {
        gos_windowing::sdl_version()
    }

    pub fn text_input_capabilities() -> String {
        to_json(&gos_windowing::window::text_input_capabilities())
    }

    pub fn has_native_menu_bar() -> bool {
        menu::has_native_menu_bar()
    }

    pub fn has_notifications() -> bool {
        gos_windowing::notify::has_notifications()
    }

    pub fn has_outbound_drag() -> bool {
        gos_windowing::drag::has_outbound_drag()
    }

    /// The Vulkan instance extensions a surface needs, encoded.
    pub fn vulkan_instance_extensions() -> Result<String, GosError> {
        gos_windowing::surface::vulkan_instance_extensions()
            .map(|v| to_json(&v))
            .map_err(fail)
    }
}

#[gos_module("windowing_window")]
mod window_bindings {
    use super::*;
    // ---- windows ----

    pub fn window_close(id: i64) -> Result<(), GosError> {
        window(id)?.close();
        Ok(())
    }

    pub fn window_title(id: i64) -> Result<String, GosError> {
        Ok(window(id)?.title())
    }

    pub fn window_set_title(id: i64, title: String) -> Result<(), GosError> {
        window(id)?.set_title(&title).map_err(fail)
    }

    pub fn window_size(id: i64) -> Result<String, GosError> {
        Ok(to_json(&window(id)?.size()))
    }

    pub fn window_set_size(id: i64, width: i64, height: i64) -> Result<(), GosError> {
        window(id)?.set_size(width, height).map_err(fail)
    }

    pub fn window_pixel_size(id: i64) -> Result<String, GosError> {
        Ok(to_json(&window(id)?.pixel_size()))
    }

    pub fn window_scale(id: i64) -> Result<f64, String> {
        Ok(window(id).map_err(|e| e.to_string())?.scale())
    }

    pub fn window_position(id: i64) -> Result<String, GosError> {
        Ok(to_json(&window(id)?.position()))
    }

    pub fn window_set_position(id: i64, x: i64, y: i64) -> Result<(), GosError> {
        window(id)?.set_position(x, y).map_err(fail)
    }

    pub fn window_set_min_size(id: i64, width: i64, height: i64) -> Result<(), GosError> {
        window(id)?.set_min_size(width, height).map_err(fail)
    }

    pub fn window_set_max_size(id: i64, width: i64, height: i64) -> Result<(), GosError> {
        window(id)?.set_max_size(width, height).map_err(fail)
    }

    pub fn window_set_resizable(id: i64, on: bool) -> Result<(), GosError> {
        window(id)?.set_resizable(on).map_err(fail)
    }

    pub fn window_set_bordered(id: i64, on: bool) -> Result<(), GosError> {
        window(id)?.set_bordered(on).map_err(fail)
    }

    pub fn window_set_fullscreen(id: i64, on: bool) -> Result<(), GosError> {
        window(id)?.set_fullscreen(on).map_err(fail)
    }

    pub fn window_set_fullscreen_mode(id: i64, mode: String) -> Result<(), GosError> {
        let mode: display::Mode = from_json(&mode, "a display mode")?;
        window(id)?.set_fullscreen_mode(mode).map_err(fail)
    }

    /// One window state flag: "fullscreen", "maximized", "minimized",
    /// "visible", "focus", "text-input", "relative-pointer".
    pub fn window_flag(id: i64, flag: String) -> Result<bool, GosError> {
        let w = window(id)?;
        Ok(match flag.as_str() {
            "fullscreen" => w.is_fullscreen(),
            "maximized" => w.is_maximized(),
            "minimized" => w.is_minimized(),
            "visible" => w.is_visible(),
            "focus" => w.has_focus(),
            "text-input" => w.is_text_input_active(),
            "relative-pointer" => w.is_relative_pointer(),
            other => return Err(GosError::from(format!("no window flag {other}"))),
        })
    }

    /// One window action without arguments: "maximize", "minimize",
    /// "restore", "show", "hide", "focus", "redraw", "start-text-input",
    /// "stop-text-input", "sync", "detach-native-tab".
    pub fn window_act(id: i64, action: String) -> Result<(), GosError> {
        let w = window(id)?;
        match action.as_str() {
            "maximize" => w.maximize(),
            "minimize" => w.minimize(),
            "restore" => w.restore(),
            "show" => w.show(),
            "hide" => w.hide(),
            "focus" => w.focus(),
            "redraw" => w.request_redraw(),
            "start-text-input" => w.start_text_input(),
            "stop-text-input" => w.stop_text_input(),
            "sync" => w.sync(),
            "detach-native-tab" => w.detach_native_tab(),
            other => return Err(GosError::from(format!("no window action {other}"))),
        }
        .map_err(fail)
    }

    pub fn window_focus_with_context(id: i64, context: String) -> Result<(), GosError> {
        let activation: instance::Activation = from_json(&context, "desktop activation context")?;
        window(id)?.focus_with_context(&activation).map_err(fail)
    }

    pub fn window_activation_token(id: i64) -> Result<String, GosError> {
        window(id)?.activation_token().map_err(fail)
    }

    pub fn window_focus_with_activation(id: i64, token: String) -> Result<(), GosError> {
        window(id)?.focus_with_activation(&token).map_err(fail)
    }

    pub fn window_set_always_on_top(id: i64, on: bool) -> Result<(), GosError> {
        window(id)?.set_always_on_top(on).map_err(fail)
    }

    pub fn window_opacity(id: i64) -> Result<f64, String> {
        Ok(window(id).map_err(|e| e.to_string())?.opacity())
    }

    pub fn window_set_opacity(id: i64, opacity: f64) -> Result<(), GosError> {
        window(id)?.set_opacity(opacity).map_err(fail)
    }

    pub fn window_request_attention(id: i64, how: String) -> Result<(), GosError> {
        let how: gos_windowing::window::Attention = from_json(&how, "an attention request")?;
        window(id)?.request_attention(how).map_err(fail)
    }

    /// Sets the parent window, or clears it with 0.
    pub fn window_set_parent(id: i64, parent: i64) -> Result<(), GosError> {
        let p = if parent == 0 {
            None
        } else {
            Some(window(parent)?)
        };
        window(id)?.set_parent(p).map_err(fail)
    }

    pub fn window_display(id: i64) -> Result<String, GosError> {
        window(id)?.display().map(|d| to_json(&d)).map_err(fail)
    }

    /// Queues a redraw of window `id`; safe from any thread.
    pub fn window_request_redraw(id: i64) -> Result<(), GosError> {
        window(id)?.request_redraw().map_err(fail)
    }

    pub fn window_start_text_input_with(id: i64, options: String) -> Result<(), GosError> {
        let options: gos_windowing::window::TextInputOptions =
            from_json(&options, "text input options")?;
        window(id)?.start_text_input_with(&options).map_err(fail)
    }

    pub fn window_set_text_input_area(
        id: i64,
        x: i64,
        y: i64,
        width: i64,
        height: i64,
        cursor: i64,
    ) -> Result<(), GosError> {
        window(id)?
            .set_text_input_area(
                display::Rect {
                    x,
                    y,
                    width,
                    height,
                },
                cursor,
            )
            .map_err(fail)
    }

    pub fn window_set_relative_pointer(id: i64, on: bool) -> Result<(), GosError> {
        window(id)?.set_relative_pointer(on).map_err(fail)
    }

    pub fn window_set_pointer_confined(id: i64, on: bool) -> Result<(), GosError> {
        window(id)?.set_pointer_confined(on).map_err(fail)
    }

    pub fn window_warp_pointer(id: i64, x: f64, y: f64) -> Result<(), GosError> {
        window(id)?.warp_pointer(x, y);
        Ok(())
    }

    pub fn window_set_icon(id: i64, rgba: Bytes, width: i64, height: i64) -> Result<(), GosError> {
        window(id)?.set_icon(&rgba.0, width, height).map_err(fail)
    }

    pub fn window_set_backdrop(id: i64, backdrop: String) -> Result<(), GosError> {
        let backdrop: gos_windowing::window::Backdrop = from_json(&backdrop, "a backdrop")?;
        window(id)?.set_backdrop(backdrop).map_err(fail)
    }

    pub fn window_set_native_tabbing(
        id: i64,
        group: String,
        enabled: bool,
    ) -> Result<(), GosError> {
        window(id)?
            .set_native_tabbing(&group, enabled)
            .map_err(fail)
    }

    pub fn window_merge_native_tab(id: i64, other: i64) -> Result<(), GosError> {
        window(id)?.merge_native_tab(window(other)?).map_err(fail)
    }

    /// Sets the frame theme: "" follows the desktop, else an encoded `Theme`.
    pub fn window_set_frame_theme(id: i64, theme: String) -> Result<(), GosError> {
        let theme: Option<system::Theme> = if theme.is_empty() {
            None
        } else {
            Some(from_json(&theme, "a theme")?)
        };
        window(id)?.set_frame_theme(theme).map_err(fail)
    }

    /// The window surface's layout: width, height, stride, pixel format.
    pub fn window_frame_info(id: i64) -> Result<String, GosError> {
        let f = window(id)?.framebuffer().map_err(fail)?;
        Ok(to_json(&FrameInfo {
            width: f.width,
            height: f.height,
            stride: f.stride,
            format: f.format,
        }))
    }

    /// Shows `pixels`, laid out as `window_frame_info` reported.
    pub fn window_present(
        id: i64,
        pixels: Bytes,
        width: i64,
        height: i64,
        stride: i64,
    ) -> Result<(), GosError> {
        let w = window(id)?;
        let mut frame = w.framebuffer().map_err(fail)?;
        if frame.width != width || frame.height != height || frame.stride != stride {
            return Err(GosError::from(format!(
                "the frame is {width}x{height} but the window is now {}x{}; take a new framebuffer after a resize",
                frame.width, frame.height
            )));
        }
        frame.pixels = pixels.0;
        w.present(&frame).map_err(fail)
    }

    /// What the window shows now.
    pub fn window_capture(id: i64) -> Result<Bytes, String> {
        let w = window(id).map_err(|e| e.to_string())?;
        w.capture()
            .map(|f| Bytes(f.pixels))
            .map_err(|e| e.to_string())
    }

    pub fn window_raw_handle(id: i64) -> Result<String, GosError> {
        use gos_windowing::surface::RawHandle as R;
        let wire = match window(id)?.raw_handle().map_err(fail)? {
            R::Win32 { hwnd, hinstance } => RawHandleWire::Win32 {
                hwnd: hwnd as u64,
                hinstance: hinstance as u64,
            },
            R::AppKit { ns_window, ns_view } => RawHandleWire::AppKit {
                ns_window: ns_window as u64,
                ns_view: ns_view as u64,
            },
            R::Wayland { display, surface } => RawHandleWire::Wayland {
                display: display as u64,
                surface: surface as u64,
            },
            R::Xlib {
                display,
                window,
                screen,
            } => RawHandleWire::Xlib {
                display: display as u64,
                window,
                screen,
            },
            R::Drm { fd, gbm_device } => RawHandleWire::Drm {
                fd,
                gbm_device: gbm_device as u64,
            },
        };
        Ok(to_json(&wire))
    }

    pub fn window_gl_context(id: i64, options: String) -> Result<i64, GosError> {
        let options: gos_windowing::surface::GlOptions = from_json(&options, "GL options")?;
        let context = window(id)?.gl_context(options).map_err(fail)?;
        Ok(state::keep_gl(context))
    }

    /// One GL context action: "make-current", "swap", "destroy".
    pub fn gl_act(handle: i64, action: String) -> Result<(), GosError> {
        state::with_gl(handle, |c| match action.as_str() {
            "make-current" => c.make_current().map_err(fail),
            "swap" => c.swap().map_err(fail),
            "destroy" => c.destroy().map_err(fail),
            other => Err(GosError::from(format!("no GL action {other}"))),
        })?;
        if action == "destroy" {
            state::drop_gl(handle);
        }
        Ok(())
    }

    pub fn gl_set_swap_interval(handle: i64, interval: i64) -> Result<(), GosError> {
        state::with_gl(handle, |c| c.set_swap_interval(interval).map_err(fail))
    }

    pub fn gl_proc_address(handle: i64, name: String) -> Result<i64, GosError> {
        state::with_gl(handle, |c| Ok(c.proc_address(&name) as i64))
    }

    pub fn window_vulkan_surface(id: i64, instance: i64) -> Result<i64, GosError> {
        window(id)?
            .vulkan_surface(instance as usize as *mut std::ffi::c_void)
            .map(|s| s as i64)
            .map_err(fail)
    }

    /// A Metal view, encoded as `[view, layer]` addresses.
    pub fn window_metal_view(id: i64) -> Result<String, GosError> {
        let view = window(id)?.metal_view().map_err(fail)?;
        Ok(to_json(&(view.view as i64, view.layer as i64)))
    }

    pub fn metal_view_destroy(view: i64) {
        // SAFETY: the address came from `window_metal_view` and names a live view.
        unsafe { sdl3_destroy_metal(view) }
    }
}

#[gos_module("windowing_desktop")]
mod desktop_bindings {
    use super::*;
    // ---- displays ----

    pub fn display_all() -> Result<String, GosError> {
        display::all().map(|d| to_json(&d)).map_err(fail)
    }

    pub fn display_primary() -> Result<String, GosError> {
        display::primary().map(|d| to_json(&d)).map_err(fail)
    }

    pub fn display_get(id: i64) -> Result<String, GosError> {
        display::get(display::DisplayId(id as u32))
            .map(|d| to_json(&d))
            .map_err(fail)
    }

    pub fn display_modes(id: i64) -> Result<String, GosError> {
        display::modes(display::DisplayId(id as u32))
            .map(|m| to_json(&m))
            .map_err(fail)
    }

    // ---- cursors ----

    pub fn cursor_system(kind: String) -> Result<i64, GosError> {
        let kind: cursor::SystemCursor = from_json(&kind, "a system cursor")?;
        Ok(state::keep_cursor(cursor::system(kind).map_err(fail)?))
    }

    pub fn cursor_custom(
        rgba: Bytes,
        width: i64,
        height: i64,
        hot_x: i64,
        hot_y: i64,
    ) -> Result<i64, GosError> {
        Ok(state::keep_cursor(
            cursor::custom(&rgba.0, width, height, hot_x, hot_y).map_err(fail)?,
        ))
    }

    pub fn cursor_set(handle: i64) -> Result<(), GosError> {
        state::cursor(handle)?.show().map_err(fail)
    }

    pub fn cursor_destroy(handle: i64) -> Result<(), GosError> {
        state::take_cursor(handle)?.destroy();
        Ok(())
    }

    pub fn cursor_show() -> Result<(), GosError> {
        cursor::show().map_err(fail)
    }

    pub fn cursor_hide() -> Result<(), GosError> {
        cursor::hide().map_err(fail)
    }

    pub fn cursor_is_visible() -> bool {
        cursor::is_visible()
    }

    // ---- clipboard ----

    pub fn clipboard_text() -> Result<String, GosError> {
        clipboard::text().map_err(fail)
    }

    pub fn clipboard_set_text(text: String) -> Result<(), GosError> {
        clipboard::set_text(&text).map_err(fail)
    }

    pub fn clipboard_has_text() -> bool {
        clipboard::has_text()
    }

    pub fn clipboard_primary_text() -> Result<String, GosError> {
        clipboard::primary_text().map_err(fail)
    }

    pub fn clipboard_set_primary_text(text: String) -> Result<(), GosError> {
        clipboard::set_primary_text(&text).map_err(fail)
    }

    pub fn clipboard_has_primary_text() -> bool {
        clipboard::has_primary_text()
    }

    /// The MIME types the clipboard holds, encoded.
    pub fn clipboard_mime_types() -> Result<String, GosError> {
        clipboard::mime_types().map(|v| to_json(&v)).map_err(fail)
    }

    pub fn clipboard_has_data(mime_type: String) -> bool {
        clipboard::has_data(&mime_type)
    }

    /// The clipboard's bytes under `mime_type`, empty when it holds none;
    /// `clipboard_has_data` tells the two apart.
    pub fn clipboard_data(mime_type: String) -> Result<Bytes, String> {
        clipboard::data(&mime_type)
            .map(|d| Bytes(d.unwrap_or_default()))
            .map_err(|e| e.to_string())
    }

    /// Offers one entry per MIME type; `data` holds every entry's bytes back
    /// to back, `lengths` each entry's length.
    pub fn clipboard_set_data(
        mime_types: Vec<String>,
        data: Bytes,
        lengths: Vec<i64>,
    ) -> Result<(), GosError> {
        clipboard::set_data(
            mime_types
                .into_iter()
                .zip(split(&data.0, &lengths)?)
                .collect(),
        )
        .map_err(fail)
    }

    pub fn clipboard_clear() -> Result<(), GosError> {
        clipboard::clear().map_err(fail)
    }

    // ---- dialogs ----

    /// Opens a file dialog of `kind` ("open", "save", "folder") from an
    /// encoded `FileDialog`; answers its id.
    pub fn dialog_cancel(id: i64) -> Result<(), GosError> {
        with_app(|app| app.cancel_dialog(dialog::DialogId(id as u64)).map_err(fail))
    }

    pub fn dialog_file(kind: String, options: String) -> Result<i64, GosError> {
        let wire: FileDialogWire = from_json(&options, "file dialog options")?;
        let options = dialog::FileDialog {
            title: wire.title,
            location: wire.location,
            filters: wire.filters,
            many: wire.many,
            parent: resolve_parent(wire.parent)?,
        };
        with_app(|app| {
            match kind.as_str() {
                "open" => app.open_file_dialog(&options),
                "save" => app.save_file_dialog(&options),
                "folder" => app.open_folder_dialog(&options),
                other => return Err(GosError::from(format!("no file dialog kind {other}"))),
            }
            .map(|id| id.0 as i64)
            .map_err(fail)
        })
    }

    pub fn dialog_message_box(
        title: String,
        message: String,
        kind: String,
    ) -> Result<(), GosError> {
        let kind: dialog::MessageKind = from_json(&kind, "a message kind")?;
        dialog::message_box(&title, &message, kind).map_err(fail)
    }

    /// The chosen button's index, or -1 when the box closed without one.
    pub fn dialog_ask(question: String) -> Result<i64, GosError> {
        let wire: QuestionWire = from_json(&question, "a question")?;
        let question = dialog::Question {
            title: wire.title,
            message: wire.message,
            buttons: wire.buttons,
            kind: wire.kind,
            parent: resolve_parent(wire.parent)?,
            default_button: wire.default_button,
            cancel_button: wire.cancel_button,
        };
        dialog::ask(&question)
            .map(|c| c.unwrap_or(-1))
            .map_err(fail)
    }

    // ---- menus and trays ----

    pub fn menu_set_bar(menus: String) -> Result<(), GosError> {
        let menus: Vec<menu::Menu> = from_json(&menus, "menus")?;
        with_app(|app| app.set_menu_bar(&menus).map_err(fail))
    }

    /// Adds a tray icon from an encoded `Option<TrayIcon>`; answers its handle.
    pub fn tray_create(icon: String, tooltip: String) -> Result<i64, GosError> {
        let icon: Option<tray::TrayIcon> = from_json(&icon, "a tray icon")?;
        let created = with_app(|app| app.tray(icon.as_ref(), &tooltip).map_err(fail))?;
        Ok(state::keep_tray(created))
    }

    pub fn tray_id(handle: i64) -> Result<i64, GosError> {
        Ok(i64::from(state::tray(handle)?.id().0))
    }

    pub fn tray_set_icon(handle: i64, icon: String) -> Result<(), GosError> {
        let icon: tray::TrayIcon = from_json(&icon, "a tray icon")?;
        state::tray(handle)?.set_icon(&icon).map_err(fail)
    }

    pub fn tray_set_tooltip(handle: i64, tooltip: String) -> Result<(), GosError> {
        state::tray(handle)?.set_tooltip(&tooltip).map_err(fail)
    }

    pub fn tray_set_menu(handle: i64, entries: String) -> Result<(), GosError> {
        let entries: Vec<menu::MenuEntry> = from_json(&entries, "menu entries")?;
        state::tray(handle)?.set_menu(&entries).map_err(fail)
    }

    pub fn tray_close(handle: i64) -> Result<(), GosError> {
        state::take_tray(handle)?.close();
        Ok(())
    }

    // ---- notifications ----

    pub fn notify(notification: String) -> Result<(), GosError> {
        let n: gos_windowing::notify::Notification = from_json(&notification, "a notification")?;
        with_app(|app| app.notify(&n).map_err(fail))
    }

    // ---- single instance ----

    /// Claims `app_id`: answers a listener handle when this process is the
    /// instance, 0 when the request went to the running one.
    pub fn instance_claim(app_id: String, request: String) -> Result<i64, GosError> {
        let request: instance::Request = from_json(&request, "an instance request")?;
        match instance::claim(&app_id, request).map_err(fail)? {
            instance::Claim::Primary(listener) => Ok(state::keep_listener(listener)),
            instance::Claim::Forwarded => Ok(0),
        }
    }

    pub fn instance_take_requests(handle: i64) -> Result<String, GosError> {
        state::with_listener(handle, |l| Ok(to_json(&l.take_requests())))
    }

    pub fn instance_take_forwarded(handle: i64) -> Result<String, GosError> {
        state::with_listener(handle, |listener| Ok(to_json(&listener.take_forwarded())))
    }

    pub fn instance_wait(handle: i64, timeout_ms: i64) -> Result<bool, GosError> {
        let timeout = std::time::Duration::from_millis(if timeout_ms < 0 {
            u64::MAX / 2
        } else {
            timeout_ms as u64
        });
        state::with_listener(handle, |listener| Ok(listener.wait(timeout)))
    }

    pub fn instance_descriptor(handle: i64) -> Result<i64, GosError> {
        state::with_listener(handle, |l| Ok(l.raw_descriptor()))
    }

    pub fn instance_close(handle: i64) -> Result<(), GosError> {
        state::take_listener(handle)?.close();
        Ok(())
    }

    pub fn file_uri(path: String) -> String {
        instance::file_uri(&path)
    }

    pub fn uri_path(uri: String) -> Option<String> {
        instance::uri_path(&uri)
    }

    // ---- drags ----

    /// Starts a drag; `data` and `lengths` hold the items' bytes as in
    /// `clipboard_set_data`.
    pub fn drag_start(
        window_id: i64,
        mime_types: Vec<String>,
        data: Bytes,
        lengths: Vec<i64>,
        operation: String,
    ) -> Result<i64, GosError> {
        let operation: gos_windowing::drag::DragOperation =
            from_json(&operation, "a drag operation")?;
        let items = mime_types
            .into_iter()
            .zip(split(&data.0, &lengths)?)
            .map(|(mime, data)| gos_windowing::drag::DragItem { mime, data })
            .collect();
        let w = window(window_id)?;
        with_app(|app| {
            app.start_drag(w, items, operation)
                .map(|d| d.0 as i64)
                .map_err(fail)
        })
    }

    // ---- desktop ----

    pub fn system_theme() -> String {
        to_json(&system::theme())
    }

    pub fn system_locales() -> Result<String, GosError> {
        system::locales().map(|l| to_json(&l)).map_err(fail)
    }

    pub fn system_open_url(url: String) -> Result<(), GosError> {
        system::open_url(&url).map_err(fail)
    }

    pub fn system_reveal(file: String) -> Result<(), GosError> {
        system::reveal(&file).map_err(fail)
    }

    pub fn system_inhibit_screensaver(inhibited: bool) -> Result<(), GosError> {
        system::inhibit_screensaver(inhibited).map_err(fail)
    }

    pub fn system_screensaver_enabled() -> bool {
        system::screensaver_enabled()
    }

    pub fn system_preferences() -> String {
        to_json(&system::preferences())
    }

    // ---- accessibility ----

    pub fn access_update(window_id: i64, elements: String) -> Result<(), GosError> {
        let elements: Vec<access::AccessElement> = from_json(&elements, "accessibility elements")?;
        access::update(window(window_id)?, elements).map_err(fail)
    }

    pub fn access_focus(window_id: i64, id: i64) -> Result<(), GosError> {
        access::focus(window(window_id)?, id as u64).map_err(fail)
    }

    pub fn access_announce(window_id: i64, text: String, assertive: bool) -> Result<(), GosError> {
        access::announce(window(window_id)?, &text, assertive).map_err(fail)
    }

    pub fn access_take_requests() -> String {
        to_json(&access::take_requests())
    }

    pub fn access_has_provider() -> bool {
        access::has_provider()
    }

    pub fn access_listening() -> bool {
        access::listening()
    }
}

/// Frees a Metal view by address.
///
/// # Safety
/// `view` must be an address `window_metal_view` answered, not yet destroyed.
unsafe fn sdl3_destroy_metal(view: i64) {
    gos_windowing::surface::MetalView {
        view: view as usize as _,
        layer: std::ptr::null_mut(),
    }
    .destroy();
}
