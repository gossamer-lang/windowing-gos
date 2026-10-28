//! What the desktop says about itself and does for the program: its theme,
//! the user's languages, opening links and folders, and the screensaver.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use sdl3_sys::locale::SDL_GetPreferredLocales;
use sdl3_sys::misc::SDL_OpenURL;
use sdl3_sys::stdinc::SDL_free;
use sdl3_sys::video::{
    SDL_DisableScreenSaver, SDL_EnableScreenSaver, SDL_GetSystemTheme, SDL_ScreenSaverEnabled,
};

use crate::{Result, support};

/// The desktop's light or dark appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Theme {
    Light,
    Dark,
    /// The platform does not say.
    Unknown,
}

/// The theme SDL's value `raw` names.
pub(crate) fn theme_from(raw: i32) -> Theme {
    match raw {
        1 => Theme::Light,
        2 => Theme::Dark,
        _ => Theme::Unknown,
    }
}

/// A language the user prefers: an ISO 639 code (`"en"`), and an ISO 3166
/// country code (`"GB"`) where one is set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Locale {
    pub language: String,
    pub country: Option<String>,
}

/// The desktop's theme now; `Event::ThemeChanged` reports changes.
pub fn theme() -> Theme {
    support::require_main_thread("windowing::system::theme");
    // SAFETY: plain query.
    theme_from(unsafe { SDL_GetSystemTheme() }.0)
}

/// The user's languages, most preferred first.
pub fn locales() -> Result<Vec<Locale>> {
    let mut count = 0;
    // SAFETY: count receives the length; SDL answers a list to free, or null.
    let list = unsafe { SDL_GetPreferredLocales(&mut count) };
    // SDL answers null when the user names no locale (`LANG` unset, or the
    // `C` locale, which SDL skips), and otherwise only when out of memory.
    if list.is_null() {
        return Ok(Vec::new());
    }
    let mut out: Vec<Locale> = Vec::new();
    // SAFETY: SDL answered `count` locale pointers inside the list's allocation.
    for entry in unsafe { std::slice::from_raw_parts(list, count as usize) } {
        if entry.is_null() {
            continue;
        }
        // SAFETY: entry is a live locale.
        let locale = unsafe { &**entry };
        let country = support::read_text(locale.country);
        let next = Locale {
            language: support::read_text(locale.language),
            country: (!country.is_empty()).then_some(country),
        };
        // SDL joins LANG and LANGUAGE on Unix, which often name one locale twice.
        if !out.contains(&next) {
            out.push(next);
        }
    }
    // SAFETY: the list was allocated for the caller to free.
    unsafe { SDL_free(list.cast::<c_void>()) };
    Ok(out)
}

/// Opens `url` in the program the desktop associates with it.
pub fn open_url(url: &str) -> Result<()> {
    let text = support::c_text(url, "a URL")?;
    // SAFETY: text outlives the call.
    support::check(unsafe { SDL_OpenURL(text.as_ptr()) }, "opening a URL")
}

/// Shows the folder holding `file` in the desktop's file manager.
pub fn reveal(file: &str) -> Result<()> {
    let path = std::path::Path::new(file);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| crate::Error::new(e.to_string()))?
            .join(path)
    };
    match absolute.parent() {
        Some(folder) => open_url(&crate::instance::file_uri(&folder.to_string_lossy())),
        None => open_url(&crate::instance::file_uri(&absolute.to_string_lossy())),
    }
}

/// Keeps the screensaver and display sleep away while `inhibited`.
pub fn inhibit_screensaver(inhibited: bool) -> Result<()> {
    support::require_main_thread("windowing::system::inhibit_screensaver");
    if inhibited {
        // SAFETY: plain call.
        support::check(
            unsafe { SDL_DisableScreenSaver() },
            "inhibiting the screensaver",
        )
    } else {
        // SAFETY: plain call.
        support::check(
            unsafe { SDL_EnableScreenSaver() },
            "allowing the screensaver",
        )
    }
}

/// Whether the screensaver may run.
pub fn screensaver_enabled() -> bool {
    support::require_main_thread("windowing::system::screensaver_enabled");
    // SAFETY: plain query.
    unsafe { SDL_ScreenSaverEnabled() }
}

/// What the desktop says about the user's appearance and accessibility
/// preferences; each is `None` where the platform does not say.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Preferences {
    /// Higher contrast than the normal theme.
    pub high_contrast: Option<bool>,
    /// Few or no animations.
    pub reduced_motion: Option<bool>,
    /// The accent color, as sRGB red, green, and blue.
    pub accent: Option<(u8, u8, u8)>,
    /// The factor the desktop scales text by beyond the display's content
    /// scale, 1.0 at the normal size.
    pub text_scale: Option<f64>,
}

// The event type `Event::PreferencesChanged` arrives as, set by `init`; 0 before.
pub(crate) static PREFERENCES_EVENT_TYPE: AtomicU32 = AtomicU32::new(0);

/// The desktop's preferences now. From the first call on, the session
/// watches them, and `Event::PreferencesChanged` says to read them again.
pub fn preferences() -> Preferences {
    support::require_main_thread("windowing::system::preferences");
    prefs::watch();
    prefs::read()
}

/// Ends the preference watch when the session ends.
pub(crate) fn shutdown() {
    prefs::stop();
}

// An sRGB channel from a component in 0..1; `None` outside it, which the
// desktop portal uses for no accent.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn channel(component: f64) -> Option<u8> {
    if !(0.0..=1.0).contains(&component) {
        return None;
    }
    Some((component * 255.0).round() as u8)
}

pub(crate) fn changed() {
    let kind = PREFERENCES_EVENT_TYPE.load(Ordering::Acquire);
    if kind != 0 {
        let _ = crate::waker::push_user_event(kind, 0, 0, 0, 0);
    }
}

// The XDG desktop portal's Settings interface, with the GNOME keys portal
// backends also carry where the portal's own appearance keys are unset.
// A thread of its own reads the `SettingChanged` signals, so a change wakes
// `App::wait` without the program running anything for it.
#[cfg(target_os = "linux")]
mod prefs {
    use std::thread::JoinHandle;

    use parking_lot::Mutex;
    use zbus::blocking::{Connection, MessageIterator};
    use zbus::zvariant::{OwnedValue, Structure, Value};

    use super::{Preferences, changed, channel};

    const DESTINATION: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const INTERFACE: &str = "org.freedesktop.portal.Settings";
    const WATCHED: [&str; 3] = [
        "org.freedesktop.appearance",
        "org.gnome.desktop.interface",
        "org.gnome.desktop.a11y.interface",
    ];

    struct Watch {
        connection: Connection,
        thread: JoinHandle<()>,
    }

    static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

    // The value the portal holds for `namespace`/`key`, `None` when it holds
    // none or no portal answers.
    fn read_one(
        connection: &Connection,
        method: &str,
        namespace: &str,
        key: &str,
    ) -> Option<OwnedValue> {
        let reply = connection
            .call_method(
                Some(DESTINATION),
                PATH,
                Some(INTERFACE),
                method,
                &(namespace, key),
            )
            .ok()?;
        let mut value: OwnedValue = reply.body().deserialize().ok()?;
        // `Read`, the version 1 method, wraps the value in one more variant.
        loop {
            let inner = match &*value {
                Value::Value(inner) => {
                    OwnedValue::try_from(inner.as_ref().try_clone().ok()?).ok()?
                }
                _ => return Some(value),
            };
            value = inner;
        }
    }

    fn setting(connection: &Connection, namespace: &str, key: &str) -> Option<OwnedValue> {
        read_one(connection, "ReadOne", namespace, key)
            .or_else(|| read_one(connection, "Read", namespace, key))
    }

    fn flag(value: Option<OwnedValue>) -> Option<bool> {
        bool::try_from(value?).ok()
    }

    // The portal's enumerations answer 1 for the preference and 0 for none.
    fn choice(value: Option<OwnedValue>) -> Option<bool> {
        match u32::try_from(value?).ok()? {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        }
    }

    fn accent(value: Option<OwnedValue>) -> Option<(u8, u8, u8)> {
        let value = value?;
        let Value::Structure(parts) = &*value else {
            return None;
        };
        let fields: Vec<f64> = Structure::fields(parts)
            .iter()
            .map(|f| f64::try_from(f.try_clone().ok()?).ok())
            .collect::<Option<Vec<f64>>>()?;
        if fields.len() != 3 {
            return None;
        }
        Some((
            channel(fields[0])?,
            channel(fields[1])?,
            channel(fields[2])?,
        ))
    }

    pub(super) fn read() -> Preferences {
        let Ok(connection) = Connection::session() else {
            return Preferences::default();
        };
        let high_contrast = choice(setting(
            &connection,
            "org.freedesktop.appearance",
            "contrast",
        ))
        .or_else(|| {
            flag(setting(
                &connection,
                "org.gnome.desktop.a11y.interface",
                "high-contrast",
            ))
        });
        let reduced_motion = choice(setting(
            &connection,
            "org.freedesktop.appearance",
            "reduced-motion",
        ))
        .or_else(|| {
            flag(setting(
                &connection,
                "org.gnome.desktop.interface",
                "enable-animations",
            ))
            .map(|on| !on)
        });
        let accent = accent(setting(
            &connection,
            "org.freedesktop.appearance",
            "accent-color",
        ));
        let text_scale = setting(
            &connection,
            "org.gnome.desktop.interface",
            "text-scaling-factor",
        )
        .and_then(|v| f64::try_from(v).ok())
        .filter(|f| *f > 0.0 && !super::x11_scale::holds_text_scale());
        Preferences {
            high_contrast,
            reduced_motion,
            accent,
            text_scale,
        }
    }

    // Subscribes before the first read, on the calling thread, so no change
    // falls between the two; the thread then reads the signals.
    pub(super) fn watch() {
        let mut watch = WATCH.lock();
        if watch.is_some() {
            return;
        }
        let Ok(connection) = Connection::session() else {
            return;
        };
        let Ok(rule) = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(INTERFACE)
            .and_then(|b| b.member("SettingChanged"))
            .map(|b| b.build())
        else {
            return;
        };
        let Ok(signals) = MessageIterator::for_match_rule(rule, &connection, Some(16)) else {
            return;
        };
        let thread = std::thread::Builder::new()
            .name("windowing-preferences".into())
            .spawn(move || {
                for message in signals {
                    let Ok(message) = message else { break };
                    let namespace: Option<String> = message
                        .body()
                        .deserialize::<(String, String, OwnedValue)>()
                        .ok()
                        .map(|b| b.0);
                    if namespace.is_some_and(|n| WATCHED.contains(&n.as_str())) {
                        changed();
                    }
                }
            });
        if let Ok(thread) = thread {
            *watch = Some(Watch { connection, thread });
        }
    }

    pub(super) fn stop() {
        let Some(w) = WATCH.lock().take() else { return };
        // Closing the connection ends the signal stream, which ends the thread.
        let _ = zbus::block_on(w.connection.inner().clone().close());
        let _ = w.thread.join();
    }
}

// Whether the X11 content scale SDL reports already holds the desktop's text
// factor. SDL takes that scale from, in order, its scaling hint, the
// `Xft.dpi` resource, the XSETTINGS window scale or `Xft/DPI`, and
// `GDK_SCALE`; desktops fold their text factor into the two DPI values only.
#[cfg(target_os = "linux")]
mod x11_scale {
    use std::collections::HashMap;
    use std::ffi::{CStr, c_char};

    use sdl3_sys::hints::SDL_GetHint;
    use x11_dl::xlib::{self, Display, XrmValue};

    use crate::xlib::{atom, xlib};

    const SETTING_INTEGER: i64 = 0;
    const SETTING_STRING: i64 = 1;
    const SETTING_COLOR: i64 = 2;

    pub(crate) fn holds_text_scale() -> bool {
        if crate::video_driver() != "x11" || forced_scale() {
            return false;
        }
        let Some(x) = xlib() else { return false };
        // SAFETY: a null name opens the default display; it is closed below.
        let display = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
        if display.is_null() {
            return false;
        }
        let holds = resource_dpi(display) > 0 || {
            let ints = xsettings(display);
            ints.get("Gdk/WindowScalingFactor").copied().unwrap_or(-1) <= 0
                && ints.get("Xft/DPI").copied().unwrap_or(-1) > 0
        };
        // SAFETY: display was opened above.
        unsafe { (x.XCloseDisplay)(display) };
        holds
    }

    fn forced_scale() -> bool {
        // SAFETY: SDL answers a string it owns, or null.
        let text = unsafe { SDL_GetHint(c"SDL_VIDEO_X11_SCALING_FACTOR".as_ptr()) };
        if text.is_null() {
            return false;
        }
        // SAFETY: a non-null hint is NUL-terminated.
        let scale = leading(&unsafe { CStr::from_ptr(text) }.to_string_lossy(), true);
        (1.0..=10.0).contains(&scale)
    }

    fn resource_dpi(display: *mut Display) -> i64 {
        let Some(x) = xlib() else { return 0 };
        // SAFETY: display is live; Xlib answers a string it owns, or null.
        let resources = unsafe { (x.XResourceManagerString)(display) };
        if resources.is_null() {
            return 0;
        }
        // SAFETY: the Xrm calls take the live resource string and free the
        // database they create before returning.
        unsafe {
            (x.XrmInitialize)();
            let database = (x.XrmGetStringDatabase)(resources);
            if database.is_null() {
                return 0;
            }
            let mut kind: *mut c_char = std::ptr::null_mut();
            let mut value = XrmValue {
                size: 0,
                addr: std::ptr::null_mut(),
            };
            let found = (x.XrmGetResource)(
                database,
                c"Xft.dpi".as_ptr(),
                c"String".as_ptr(),
                &mut kind,
                &mut value,
            ) != 0;
            let dpi = if found
                && !kind.is_null()
                && !value.addr.is_null()
                && CStr::from_ptr(kind).to_bytes() == b"String"
            {
                leading(&CStr::from_ptr(value.addr).to_string_lossy(), false) as i64
            } else {
                0
            };
            (x.XrmDestroyDatabase)(database);
            dpi
        }
    }

    unsafe extern "C" fn ignore_error(
        _display: *mut Display,
        _event: *mut xlib::XErrorEvent,
    ) -> i32 {
        0
    }

    // The integer settings the XSETTINGS manager of the default screen
    // publishes; empty when none runs.
    fn xsettings(display: *mut Display) -> HashMap<String, i64> {
        let Some(x) = xlib() else {
            return HashMap::new();
        };
        // SAFETY: display is live.
        let screen = unsafe { (x.XDefaultScreen)(display) };
        // SAFETY: display is live.
        let owner = unsafe {
            (x.XGetSelectionOwner)(display, atom(x, display, &format!("_XSETTINGS_S{screen}")))
        };
        if owner == 0 {
            return HashMap::new();
        }
        let property = atom(x, display, "_XSETTINGS_SETTINGS");
        let (mut kind, mut bits, mut count, mut after) = (0, 0, 0, 0);
        let mut data: *mut u8 = std::ptr::null_mut();
        // The manager may exit between the two requests; its window is then
        // gone, which Xlib's default handler would treat as fatal.
        // SAFETY: display is live; the outputs are valid storage; the previous
        // handler is restored before returning.
        let status = unsafe {
            let previous = (x.XSetErrorHandler)(Some(ignore_error));
            let status = (x.XGetWindowProperty)(
                display,
                owner,
                property,
                0,
                1 << 24,
                0,
                0,
                &mut kind,
                &mut bits,
                &mut count,
                &mut after,
                &mut data,
            );
            (x.XSync)(display, 0);
            (x.XSetErrorHandler)(previous);
            status
        };
        if data.is_null() {
            return HashMap::new();
        }
        // SAFETY: Xlib answered `count` bytes of a format-8 property.
        let bytes = unsafe { std::slice::from_raw_parts(data, count as usize) }.to_vec();
        // SAFETY: Xlib allocated data for the caller to free.
        unsafe { (x.XFree)(data.cast()) };
        if status != 0 || bits != 8 {
            return HashMap::new();
        }
        integers(&bytes)
    }

    // The XSETTINGS layout: a byte-order byte, three pad bytes, a serial and
    // a count, then per setting its type, a pad byte, the name's length and
    // name, a serial, and the value, each part padded to four bytes.
    fn integers(bytes: &[u8]) -> HashMap<String, i64> {
        let mut ints = HashMap::new();
        if bytes.len() < 12 {
            return ints;
        }
        let big = bytes[0] == 1;
        let Some(settings) = number(bytes, 8, 4, big) else {
            return ints;
        };
        let mut at = 12;
        for _ in 0..settings {
            let (Some(kind), Some(length)) =
                (number(bytes, at, 1, big), number(bytes, at + 2, 2, big))
            else {
                break;
            };
            let name_end = at + 4 + length;
            if name_end > bytes.len() as i64 {
                break;
            }
            let name = String::from_utf8(bytes[(at + 4) as usize..name_end as usize].to_vec())
                .unwrap_or_default();
            at += 4 + padded(length) + 4;
            match kind {
                SETTING_INTEGER => {
                    let Some(value) = number(bytes, at, 4, big) else {
                        break;
                    };
                    ints.insert(
                        name,
                        if value >= 1 << 31 {
                            value - (1 << 32)
                        } else {
                            value
                        },
                    );
                    at += 4;
                }
                SETTING_STRING => {
                    let Some(length) = number(bytes, at, 4, big) else {
                        break;
                    };
                    at += 4 + padded(length);
                }
                SETTING_COLOR => at += 8,
                _ => break,
            }
        }
        ints
    }

    fn padded(length: i64) -> i64 {
        (length + 3) / 4 * 4
    }

    // The unsigned `width`-byte number at `at`, `None` past the end.
    fn number(bytes: &[u8], at: i64, width: i64, big: bool) -> Option<i64> {
        if at + width > bytes.len() as i64 {
            return None;
        }
        let mut value = 0i64;
        for i in 0..width {
            let b = i64::from(bytes[(if big { at + i } else { at + width - 1 - i }) as usize]);
            value = (value << 8) | b;
        }
        Some(value)
    }

    // The number C's `atof` (or `atoi`, without `fraction`) reads at the
    // start of `text`; 0 when there is none.
    fn leading(text: &str, fraction: bool) -> f64 {
        let body = text.trim_start();
        let end = body
            .char_indices()
            .take_while(|(i, c)| {
                c.is_ascii_digit()
                    || (*i == 0 && (*c == '-' || *c == '+'))
                    || (fraction && *c == '.')
            })
            .count();
        body[..end].parse().unwrap_or(0.0)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn xsettings_integers_parse_in_either_byte_order() {
            // One integer setting "Xft/DPI" = 98304 (96 * 1024), little-endian.
            let mut bytes = vec![0u8, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
            bytes.extend_from_slice(&[0, 0, 7, 0]);
            bytes.extend_from_slice(b"Xft/DPI\0");
            bytes.extend_from_slice(&[0, 0, 0, 0]);
            bytes.extend_from_slice(&98304u32.to_le_bytes());
            assert_eq!(super::integers(&bytes).get("Xft/DPI"), Some(&98304));
            assert_eq!(super::leading("  96.5dpi", true), 96.5);
            assert_eq!(super::leading("96.5", false), 96.0);
        }
    }
}

// SystemParametersInfo for contrast and animations, the DWM colorization
// color for the accent, and the Accessibility text scale. Changes arrive as
// window messages, which SDL's message hook passes on.
#[cfg(target_os = "windows")]
mod prefs {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};

    use sdl3_sys::system::{MSG, SDL_SetWindowsMessageHook};
    use windows_sys::Win32::Graphics::Dwm::DwmGetColorizationColor;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SystemParametersInfoW,
        WM_DWMCOLORIZATIONCOLORCHANGED, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED,
    };

    use super::{Preferences, changed};

    static HOOKED: AtomicBool = AtomicBool::new(false);

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn read() -> Preferences {
        let mut contrast = HIGHCONTRASTW {
            cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
            dwFlags: 0,
            lpszDefaultScheme: std::ptr::null_mut(),
        };
        // SAFETY: contrast is valid storage of the size given.
        let high_contrast = (unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                contrast.cbSize,
                (&mut contrast as *mut HIGHCONTRASTW).cast(),
                0,
            )
        } != 0)
            .then_some(contrast.dwFlags & HCF_HIGHCONTRASTON != 0);
        let mut animate: i32 = 1;
        // SAFETY: animate is valid storage for a BOOL.
        let reduced_motion = (unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animate as *mut i32).cast(),
                0,
            )
        } != 0)
            .then_some(animate == 0);
        let (mut color, mut opaque) = (0u32, 0i32);
        // SAFETY: both outputs are valid storage.
        let accent =
            (unsafe { DwmGetColorizationColor(&mut color, &mut opaque) } >= 0).then_some((
                ((color >> 16) & 0xFF) as u8,
                ((color >> 8) & 0xFF) as u8,
                (color & 0xFF) as u8,
            ));
        let (mut percent, mut size) = (0u32, 4u32);
        let key = wide("Software\\Microsoft\\Accessibility");
        let value = wide("TextScaleFactor");
        // An unset scale is the normal size.
        // SAFETY: the strings are NUL-terminated; percent holds a DWORD.
        let text_scale = if unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut percent as *mut u32).cast(),
                &mut size,
            )
        } == 0
        {
            Some(f64::from(percent) / 100.0)
        } else {
            Some(1.0)
        };
        Preferences {
            high_contrast,
            reduced_motion,
            accent,
            text_scale,
        }
    }

    unsafe extern "C" fn on_message(_userdata: *mut c_void, message: *mut MSG) -> bool {
        // SAFETY: SDL hands a complete message.
        let m = unsafe { &*message.cast::<windows_sys::Win32::UI::WindowsAndMessaging::MSG>() };
        if matches!(
            m.message,
            WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED
        ) {
            changed();
        }
        true
    }

    pub(super) fn watch() {
        if !HOOKED.swap(true, Ordering::AcqRel) {
            // SAFETY: the hook stays valid for the program's life.
            unsafe { SDL_SetWindowsMessageHook(Some(on_message), std::ptr::null_mut()) };
        }
    }

    pub(super) fn stop() {
        if HOOKED.swap(false, Ordering::AcqRel) {
            // SAFETY: clears the hook SDL calls.
            unsafe { SDL_SetWindowsMessageHook(None, std::ptr::null_mut()) };
        }
    }
}

// NSWorkspace's accessibility display options and NSColor's accent color.
#[cfg(target_os = "macos")]
mod prefs {
    use super::Preferences;

    pub(super) fn read() -> Preferences {
        crate::macos::preferences()
    }

    pub(super) fn watch() {
        crate::macos::watch_preferences();
    }

    pub(super) fn stop() {}
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod prefs {
    use super::Preferences;

    pub(super) fn read() -> Preferences {
        Preferences::default()
    }

    pub(super) fn watch() {}

    pub(super) fn stop() {}
}
