//! Native menus: one model for the macOS menu bar and the system tray. The
//! program owns item identities and state; a choice arrives as
//! `Event::MenuSelected`, after which the program updates its model and
//! installs it again.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::event::Mods;
use crate::keys::Key;
use crate::window::WindowId;
use crate::{App, Result};

/// A keyboard shortcut a menu item shows and, on macOS, answers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shortcut {
    pub key: Key,
    pub mods: Mods,
}

/// One entry of a native menu. Identities are the program's own and must be
/// unique within the menus installed together.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MenuEntry {
    Action {
        id: u32,
        title: String,
        enabled: bool,
        shortcut: Option<Shortcut>,
    },
    /// A check item; the platform shows `checked` and leaves toggling to the program.
    Check {
        id: u32,
        title: String,
        enabled: bool,
        checked: bool,
        shortcut: Option<Shortcut>,
    },
    /// A member of the exclusive set `group`; the program checks exactly one.
    Radio {
        id: u32,
        title: String,
        enabled: bool,
        checked: bool,
        group: u32,
        shortcut: Option<Shortcut>,
    },
    Separator,
    Submenu {
        title: String,
        enabled: bool,
        entries: Vec<MenuEntry>,
    },
}

/// A titled top-level menu of a menu bar.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Menu {
    pub title: String,
    pub entries: Vec<MenuEntry>,
}

/// A tray icon the application created, as `MenuSource::Tray` names it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct TrayId(pub u32);

/// Where a chosen menu item lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MenuSource {
    /// The menu bar; `window` is the key window when the choice was made, if any.
    MenuBar {
        window: Option<WindowId>,
    },
    Tray {
        tray: TrayId,
    },
}

/// The event type native menu callbacks queue, set when a tray or menu bar is
/// installed. The callbacks carry only a context word or an item tag.
pub(crate) static MENU_EVENT_TYPE: AtomicU32 = AtomicU32::new(0);

impl App {
    /// Installs `menus` in the macOS menu bar, replacing the program's
    /// earlier ones; a choice arrives as `Event::MenuSelected` with
    /// `MenuSource::MenuBar`. Other platforms have no native menu bar and
    /// answer an error: draw the menus in the window there, as
    /// `has_native_menu_bar` tells.
    pub fn set_menu_bar(&self, menus: &[Menu]) -> Result<()> {
        crate::support::require_main_thread("App::set_menu_bar");
        MENU_EVENT_TYPE.store(
            self.event_base + crate::event::MENU_OFFSET,
            Ordering::Release,
        );
        crate::macos::install_menu_bar(menus)
    }
}

/// Whether the platform shows a native menu bar `App::set_menu_bar` fills.
pub fn has_native_menu_bar() -> bool {
    cfg!(target_os = "macos")
}
