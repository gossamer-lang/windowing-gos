//! System tray icons (the notification area on Windows, status items on
//! macOS, StatusNotifierItem on Linux) with a native menu. A chosen item
//! arrives as `Event::MenuSelected` with `MenuSource::Tray`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use sdl3_sys::surface::{SDL_DestroySurface, SDL_Surface};
use sdl3_sys::tray::*;

use crate::menu::{self, MenuEntry, TrayId};
use crate::{App, Result, event, support, surface, waker};

/// A tray icon's pixels.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrayIcon {
    pub rgba: Vec<u8>,
    pub width: i64,
    pub height: i64,
}

/// A tray icon the application created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tray {
    handle: *mut SDL_Tray,
    id: TrayId,
}

// SAFETY: a Tray is an identity; every call that uses it checks the main thread.
unsafe impl Send for Tray {}

// Tray identities count from 1, so an entry's context word is never zero.
// SDL's tray callbacks carry only that word, which holds the tray and item
// identities.
static NEXT_TRAY: AtomicU32 = AtomicU32::new(1);

unsafe extern "C" fn entry_chosen(context: *mut c_void, _entry: *mut SDL_TrayEntry) {
    let code = context as u64;
    let tray = code >> 32;
    let item = (code & 0xFFFF_FFFF) as u32;
    // A tray callback answers nothing; a refused event is a lost click.
    let _ = waker::push_user_event(
        menu::MENU_EVENT_TYPE.load(Ordering::Acquire),
        0,
        item as i32,
        tray,
        0,
    );
}

fn icon_surface(icon: Option<&TrayIcon>) -> Result<*mut SDL_Surface> {
    match icon {
        Some(picture) => surface::rgba_surface(&picture.rgba, picture.width, picture.height),
        None => Ok(std::ptr::null_mut()),
    }
}

impl App {
    /// Adds an icon to the system tray.
    pub fn tray(&self, icon: Option<&TrayIcon>, tooltip: &str) -> Result<Tray> {
        support::require_main_thread("App::tray");
        menu::MENU_EVENT_TYPE.store(self.event_base + event::MENU_OFFSET, Ordering::Release);
        let picture = icon_surface(icon)?;
        let text = support::c_text(tooltip, "a tray tooltip");
        // SAFETY: picture is null or live; text outlives the call.
        let created = text.map(|t| unsafe { SDL_CreateTray(picture, t.as_ptr()) });
        if !picture.is_null() {
            // SAFETY: SDL copied the icon.
            unsafe { SDL_DestroySurface(picture) };
        }
        let handle = created?;
        if handle.is_null() {
            return Err(support::failure("creating a tray icon"));
        }
        let id = NEXT_TRAY.fetch_add(1, Ordering::Relaxed);
        Ok(Tray {
            handle,
            id: TrayId(id),
        })
    }
}

impl Tray {
    pub fn id(&self) -> TrayId {
        self.id
    }

    pub fn set_icon(&self, icon: &TrayIcon) -> Result<()> {
        support::require_main_thread("Tray::set_icon");
        let picture = surface::rgba_surface(&icon.rgba, icon.width, icon.height)?;
        // SAFETY: handle and picture are live; SDL copies the icon.
        unsafe {
            SDL_SetTrayIcon(self.handle, picture);
            SDL_DestroySurface(picture);
        }
        Ok(())
    }

    pub fn set_tooltip(&self, tooltip: &str) -> Result<()> {
        support::require_main_thread("Tray::set_tooltip");
        let text = support::c_text(tooltip, "a tray tooltip")?;
        // SAFETY: handle is live; SDL copies the text.
        unsafe { SDL_SetTrayTooltip(self.handle, text.as_ptr()) };
        Ok(())
    }

    /// Replaces the tray's menu with `entries`.
    pub fn set_menu(&self, entries: &[MenuEntry]) -> Result<()> {
        support::require_main_thread("Tray::set_menu");
        // SAFETY: handle is live.
        let existing = unsafe { SDL_GetTrayMenu(self.handle) };
        let menu = if existing.is_null() {
            // SAFETY: handle is live.
            let created = unsafe { SDL_CreateTrayMenu(self.handle) };
            if created.is_null() {
                return Err(support::failure("creating a tray menu"));
            }
            created
        } else {
            clear(existing);
            existing
        };
        fill(menu, entries, self.id)
    }

    pub fn close(&self) {
        support::require_main_thread("Tray::close");
        // SAFETY: handle is live and not used after this.
        unsafe { SDL_DestroyTray(self.handle) }
    }
}

fn clear(menu: *mut SDL_TrayMenu) {
    loop {
        let mut count = 0;
        // SAFETY: menu is live; SDL owns the list.
        let list = unsafe { SDL_GetTrayEntries(menu, &mut count) };
        if list.is_null() || count == 0 {
            return;
        }
        // SAFETY: the list holds `count` > 0 entries.
        let entry = unsafe { *list };
        if entry.is_null() {
            return;
        }
        // SAFETY: entry is live.
        unsafe { SDL_RemoveTrayEntry(entry.cast_mut()) };
    }
}

fn fill(menu: *mut SDL_TrayMenu, entries: &[MenuEntry], tray: TrayId) -> Result<()> {
    for entry in entries {
        match entry {
            MenuEntry::Separator => {
                // SAFETY: menu is live; a null label inserts a separator.
                let item = unsafe {
                    SDL_InsertTrayEntryAt(menu, -1, std::ptr::null(), SDL_TRAYENTRY_BUTTON)
                };
                if item.is_null() {
                    return Err(support::failure("adding a tray separator"));
                }
            }
            MenuEntry::Submenu {
                title,
                enabled,
                entries,
            } => {
                let flags = SDL_TRAYENTRY_SUBMENU.0
                    | if *enabled {
                        0
                    } else {
                        SDL_TRAYENTRY_DISABLED.0
                    };
                let label = support::c_text(title, "a tray menu title")?;
                // SAFETY: menu is live; SDL copies the label.
                let item = unsafe {
                    SDL_InsertTrayEntryAt(menu, -1, label.as_ptr(), SDL_TrayEntryFlags(flags))
                };
                if item.is_null() {
                    return Err(support::failure("adding a tray submenu"));
                }
                // SAFETY: item is live.
                let child = unsafe { SDL_CreateTraySubmenu(item) };
                if child.is_null() {
                    return Err(support::failure("creating a tray submenu"));
                }
                fill(child, entries, tray)?;
            }
            MenuEntry::Action {
                id, title, enabled, ..
            } => add_item(menu, tray, *id, title, *enabled, None)?,
            MenuEntry::Check {
                id,
                title,
                enabled,
                checked,
                ..
            }
            | MenuEntry::Radio {
                id,
                title,
                enabled,
                checked,
                ..
            } => {
                add_item(menu, tray, *id, title, *enabled, Some(*checked))?;
            }
        }
    }
    Ok(())
}

fn add_item(
    menu: *mut SDL_TrayMenu,
    tray: TrayId,
    id: u32,
    title: &str,
    enabled: bool,
    checked: Option<bool>,
) -> Result<()> {
    let kind = match checked {
        Some(true) => SDL_TRAYENTRY_CHECKBOX.0 | SDL_TRAYENTRY_CHECKED.0,
        Some(false) => SDL_TRAYENTRY_CHECKBOX.0,
        None => SDL_TRAYENTRY_BUTTON.0,
    };
    let flags = kind | if enabled { 0 } else { SDL_TRAYENTRY_DISABLED.0 };
    let label = support::c_text(title, "a tray menu item")?;
    // SAFETY: menu is live; SDL copies the label.
    let item =
        unsafe { SDL_InsertTrayEntryAt(menu, -1, label.as_ptr(), SDL_TrayEntryFlags(flags)) };
    if item.is_null() {
        return Err(support::failure("adding a tray menu item"));
    }
    let code = (u64::from(tray.0) << 32) | u64::from(id);
    // SAFETY: item is live; the context is a plain word, never dereferenced.
    unsafe { SDL_SetTrayEntryCallback(item, Some(entry_chosen), code as usize as *mut c_void) };
    Ok(())
}
