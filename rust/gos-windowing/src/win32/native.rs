// Desktop Window Manager calls SDL leaves to the program, made on the
// window's HWND, and notification-area balloons.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, ICON_SMALL2, IDI_APPLICATION, LoadIconW,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW,
    SetWindowPos, WM_GETICON, WM_NCACTIVATE,
};

use crate::{Error, Result};

// `DWMWA_SYSTEMBACKDROP_TYPE`, which Windows 11 22H2 introduced.
const SYSTEM_BACKDROP_TYPE: u32 = 38;
// `DWMWA_USE_IMMERSIVE_DARK_MODE`; builds before Windows 10 20H1 read the
// same switch from attribute 19.
const IMMERSIVE_DARK_MODE: u32 = 20;
const IMMERSIVE_DARK_MODE_BEFORE_20H1: u32 = 19;

fn set_attribute(hwnd: *mut c_void, attribute: u32, value: i32) -> i32 {
    // SAFETY: `hwnd` is a live window SDL created; DWM reads 4 bytes of `value`.
    unsafe { DwmSetWindowAttribute(hwnd as HWND, attribute, (&raw const value).cast(), 4) }
}

/// Sets the system backdrop of `hwnd` to the `DWMSBT_*` value `kind`.
pub(crate) fn set_backdrop(hwnd: *mut c_void, kind: i32) -> Result<()> {
    let result = set_attribute(hwnd, SYSTEM_BACKDROP_TYPE, kind);
    if result < 0 {
        return Err(Error::new(format!(
            "setting the window backdrop: DWM answered 0x{:08x} (it needs Windows 11 22H2 or later)",
            result as u32
        )));
    }
    Ok(())
}

/// Draws the title bar and borders of `hwnd` dark or light.
pub(crate) fn set_dark_frame(hwnd: *mut c_void, dark: bool) -> Result<()> {
    let value = i32::from(dark);
    let mut result = set_attribute(hwnd, IMMERSIVE_DARK_MODE, value);
    if result < 0 {
        result = set_attribute(hwnd, IMMERSIVE_DARK_MODE_BEFORE_20H1, value);
    }
    if result < 0 {
        return Err(Error::new(format!(
            "setting the frame theme: DWM answered 0x{:08x} (it needs Windows 10 1809 or later)",
            result as u32
        )));
    }
    // Windows 11 repaints the frame at once; Windows 10 repaints the caption
    // when the window's activation is drawn again.
    let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
    // SAFETY: `hwnd` is a live window SDL created, on its own thread.
    unsafe {
        SetWindowPos(hwnd as HWND, std::ptr::null_mut(), 0, 0, 0, 0, flags);
        if std::ptr::eq(GetActiveWindow(), hwnd as HWND) {
            SendMessageW(hwnd as HWND, WM_NCACTIVATE, 0, 0);
            SendMessageW(hwnd as HWND, WM_NCACTIVATE, 1, 0);
        }
    }
    Ok(())
}

/// Whether `hwnd` is on the virtual desktop the user is on; true when the
/// desktop does not answer (Windows before 10, or no shell).
pub(crate) fn on_current_desktop(hwnd: *mut c_void) -> bool {
    use windows::Win32::Foundation::HWND as Hwnd;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
    // COM may already run on this thread (SDL starts OLE for drops); a
    // repeat start succeeds and one in another mode still lets COM answer.
    // SAFETY: called on the window's own thread; `hwnd` is SDL's live window.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(manager) =
            CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL)
        else {
            return true;
        };
        manager
            .IsWindowOnCurrentVirtualDesktop(Hwnd(hwnd))
            .map_or(true, |on| on.as_bool())
    }
}

/// The visible frame around `hwnd`'s client area in pixels: top, left,
/// bottom and right. DWM draws the frame inside the window rectangle, whose
/// outer band is an invisible resize border, so the frame comes from DWM's
/// extended frame bounds.
pub(crate) fn frame_borders(hwnd: *mut c_void) -> Option<(i64, i64, i64, i64)> {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let mut client = frame;
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: `hwnd` is SDL's live window; each output is valid storage of
    // the size passed.
    let read = unsafe {
        DwmGetWindowAttribute(
            hwnd as HWND,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&raw mut frame).cast(),
            std::mem::size_of::<RECT>() as u32,
        ) >= 0
            && GetClientRect(hwnd as HWND, &mut client) != 0
            && ClientToScreen(hwnd as HWND, &mut origin) != 0
    };
    if !read {
        return None;
    }
    let (wide, tall) = (i64::from(client.right), i64::from(client.bottom));
    let top = i64::from(origin.y - frame.top);
    let left = i64::from(origin.x - frame.left);
    let bottom = i64::from(frame.bottom) - (i64::from(origin.y) + tall);
    let right = i64::from(frame.right) - (i64::from(origin.x) + wide);
    Some((top.max(0), left.max(0), bottom.max(0), right.max(0)))
}

// The message-only window that owns the notification icon, and whether the
// icon is in the notification area. Both belong to the main thread.
static OWNER: AtomicIsize = AtomicIsize::new(0);
static ADDED: AtomicBool = AtomicBool::new(false);

fn fill<const N: usize>(text: &str) -> [u16; N] {
    let mut slots = [0u16; N];
    for (slot, unit) in slots.iter_mut().take(N - 1).zip(text.encode_utf16()) {
        *slot = unit;
    }
    slots
}

fn icon_data() -> NOTIFYICONDATAW {
    // SAFETY: an all-zero NOTIFYICONDATAW is its documented empty value.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = OWNER.load(Ordering::Relaxed) as HWND;
    data.uID = 1;
    data
}

/// Shows a notification as a notification-area balloon, which Windows 10
/// and later present as a toast. Later notifications reuse the icon, the
/// application's when one of `windows` carries one.
pub(crate) fn show_notification(
    title: &str,
    body: &str,
    severity: u32,
    windows: &[*mut c_void],
) -> Result<()> {
    if OWNER.load(Ordering::Relaxed) == 0 {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: a message-only STATIC window with no parent data.
        let owner = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                [0u16].as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if owner.is_null() {
            return Err(Error::new("cannot create the notification window"));
        }
        OWNER.store(owner as isize, Ordering::Relaxed);
    }
    // SDL gives every window the application's icon.
    let own = windows.iter().find_map(|&w| {
        // SAFETY: each handle is a live window SDL created.
        let icon = unsafe { SendMessageW(w as HWND, WM_GETICON, ICON_SMALL2 as usize, 0) };
        (icon != 0).then_some(icon)
    });
    // SAFETY: the stock application icon needs no instance.
    let icon = own.map_or_else(
        || unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) },
        |i| i as _,
    );
    let mut data = icon_data();
    data.uFlags = NIF_ICON | NIF_TIP | NIF_INFO;
    data.hIcon = icon;
    data.szTip = fill::<128>(title);
    data.szInfo = fill::<256>(body);
    data.szInfoTitle = fill::<64>(title);
    data.dwInfoFlags = match severity {
        2 => NIIF_ERROR,
        1 => NIIF_WARNING,
        _ => NIIF_INFO,
    };
    let message = if ADDED.load(Ordering::Relaxed) {
        NIM_MODIFY
    } else {
        NIM_ADD
    };
    // SAFETY: `data` is a filled NOTIFYICONDATAW for the owner window.
    if unsafe { Shell_NotifyIconW(message, &data) } == 0 {
        return Err(Error::new("Windows refused the notification"));
    }
    ADDED.store(true, Ordering::Relaxed);
    Ok(())
}

/// Removes the notification icon and its window.
pub(crate) fn shutdown() {
    if ADDED.swap(false, Ordering::Relaxed) {
        // SAFETY: deletes the icon this module added.
        unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data()) };
    }
    let owner = OWNER.swap(0, Ordering::Relaxed);
    if owner != 0 {
        // SAFETY: the window this module created.
        unsafe { DestroyWindow(owner as HWND) };
    }
}
