// AppKit calls SDL leaves to the program: window tabs, the menu bar, frame
// appearance, notifications, desktop preferences, and drags. Everything here
// runs on the main thread, where SDL runs AppKit.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSApplication, NSColor, NSColorSpace, NSDragOperation,
    NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent,
    NSEventModifierFlags, NSEventType, NSFont, NSImage, NSMenu, NSMenuItem, NSPasteboardItem,
    NSPasteboardWriting, NSView, NSWindow, NSWindowTabbingMode, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSNotification, NSNotificationCenter, NSPoint, NSProcessInfo, NSRect, NSSize,
    NSString, NSURL,
};

use crate::drag::{DragId, DragItem, DragOperation, operation_code};
use crate::menu::{Menu, MenuEntry, Shortcut};
use crate::window::Window;
use crate::{Error, Result};

fn main_thread() -> Result<MainThreadMarker> {
    MainThreadMarker::new().ok_or_else(|| Error::new("AppKit calls need the main thread"))
}

// SAFETY: callers pass an `NSWindow *` SDL created, alive for the call.
unsafe fn window<'a>(ns_window: *mut c_void) -> Option<&'a NSWindow> {
    unsafe { (ns_window as *const NSWindow).as_ref() }
}

/// The content view of the `NSWindow` at `ns_window`.
pub(crate) fn content_view(ns_window: *mut c_void) -> *mut c_void {
    // SAFETY: `ns_window` is SDL's live window; the view outlives the call.
    unsafe { window(ns_window) }
        .and_then(|w| w.contentView())
        .map_or(std::ptr::null_mut(), |v| {
            Retained::as_ptr(&v) as *mut c_void
        })
}

/// Configures explicit AppKit tabbing for windows with the same identifier.
pub(crate) fn configure_tabs(
    ns_window: *mut c_void,
    identifier: &str,
    enabled: bool,
) -> Result<()> {
    // SAFETY: SDL's live window.
    let w = unsafe { window(ns_window) }.ok_or_else(|| Error::new("no Cocoa window"))?;
    w.setTabbingIdentifier(&NSString::from_str(identifier));
    w.setTabbingMode(if enabled {
        NSWindowTabbingMode::Preferred
    } else {
        NSWindowTabbingMode::Disallowed
    });
    Ok(())
}

/// Adds a window to another window's AppKit tab group.
pub(crate) fn merge_tabs(ns_window: *mut c_void, other: *mut c_void) -> Result<()> {
    // SAFETY: both are SDL's live windows.
    let (Some(w), Some(o)) = (unsafe { window(ns_window) }, unsafe { window(other) }) else {
        return Err(Error::new("no Cocoa window"));
    };
    w.addTabbedWindow_ordered(o, objc2_app_kit::NSWindowOrderingMode::Above);
    Ok(())
}

/// Removes a window from its AppKit tab group while keeping the window open.
pub(crate) fn detach_tab(ns_window: *mut c_void) -> Result<()> {
    // SAFETY: SDL's live window.
    let w = unsafe { window(ns_window) }.ok_or_else(|| Error::new("no Cocoa window"))?;
    if let Some(group) = w.tabGroup() {
        group.removeWindow(w);
    }
    Ok(())
}

// The menu bar. Each program item is an `NSMenuItem` whose tag is the
// program's identity and whose target is one shared `GosMenuTarget`, whose
// `choose:` queues `Event::MenuSelected`. The top-level items the program
// installed carry `MENU_BAR_TAG`, so a later install replaces them and keeps
// the application and Window menus SDL made.
const MENU_BAR_TAG: isize = 0x676F73;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GosMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(choose:))]
        fn choose(&self, sender: Option<&NSMenuItem>) {
            let Some(item) = sender else { return };
            let window = key_window_id().unwrap_or(0);
            let kind = crate::menu::MENU_EVENT_TYPE.load(Ordering::Relaxed);
            // AppKit's action answers nothing; a refused event is a lost choice.
            let _ = crate::waker::push_user_event(kind, window, item.tag() as i32, 0, 0);
        }
    }
);

thread_local! {
    // The target lives as long as the menu bar, which is the process.
    static MENU_TARGET: std::cell::OnceCell<Retained<MenuTarget>> = const { std::cell::OnceCell::new() };
}

fn menu_target(mtm: MainThreadMarker) -> Retained<MenuTarget> {
    MENU_TARGET.with(|t| {
        t.get_or_init(|| {
            // SAFETY: NSObject's init on a fresh allocation.
            unsafe { msg_send![MenuTarget::alloc(mtm), init] }
        })
        .clone()
    })
}

// The SDL identity of AppKit's key window, if it is one of the program's.
fn key_window_id() -> Option<u32> {
    let mtm = MainThreadMarker::new()?;
    let key = NSApplication::sharedApplication(mtm).keyWindow()?;
    crate::window::sdl_id_of_cocoa_window(Retained::as_ptr(&key) as *mut c_void)
}

// `key` as an AppKit key equivalent: printable keys are their character, and
// the keys AppKit names by function-key code points use those.
fn key_equivalent(shortcut: &Option<Shortcut>) -> (String, NSEventModifierFlags) {
    use crate::keys::Key;
    let Some(s) = shortcut else {
        return (String::new(), NSEventModifierFlags::empty());
    };
    let text = match s.key {
        Key::Up => "\u{F700}".to_string(),
        Key::Down => "\u{F701}".to_string(),
        Key::Left => "\u{F702}".to_string(),
        Key::Right => "\u{F703}".to_string(),
        Key::Return => "\r".to_string(),
        Key::Tab => "\t".to_string(),
        Key::Escape => "\u{1B}".to_string(),
        Key::Backspace => "\u{8}".to_string(),
        Key::Delete => "\u{7F}".to_string(),
        k if k.code() < 0x4000_0000 => char::from_u32(k.code())
            .map(String::from)
            .unwrap_or_default(),
        _ => String::new(),
    };
    let mut mask = NSEventModifierFlags::empty();
    if s.mods.shift {
        mask |= NSEventModifierFlags::Shift;
    }
    if s.mods.ctrl {
        mask |= NSEventModifierFlags::Control;
    }
    if s.mods.alt {
        mask |= NSEventModifierFlags::Option;
    }
    if s.mods.gui {
        mask |= NSEventModifierFlags::Command;
    }
    (text, mask)
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    key: &str,
    tag: isize,
    enabled: bool,
) -> Retained<NSMenuItem> {
    // SAFETY: a plain item; a selector the target implements, or none.
    let made = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    };
    made.setTag(tag);
    made.setEnabled(enabled);
    made
}

fn new_menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    // The program's `enabled` flags decide, not AppKit's responder chain.
    menu.setAutoenablesItems(false);
    menu
}

fn fill_menu(mtm: MainThreadMarker, menu: &NSMenu, entries: &[MenuEntry]) {
    let action =
        |id: u32, title: &str, enabled: bool, checked: bool, shortcut: &Option<Shortcut>| {
            let (key, mask) = key_equivalent(shortcut);
            let it = item(mtm, title, Some(sel!(choose:)), &key, id as isize, enabled);
            // SAFETY: the target outlives every menu item.
            unsafe { it.setTarget(Some(&menu_target(mtm))) };
            it.setState(if checked { 1 } else { 0 });
            it.setKeyEquivalentModifierMask(mask);
            menu.addItem(&it);
        };
    for entry in entries {
        match entry {
            MenuEntry::Action {
                id,
                title,
                enabled,
                shortcut,
            } => action(*id, title, *enabled, false, shortcut),
            MenuEntry::Check {
                id,
                title,
                enabled,
                checked,
                shortcut,
            }
            | MenuEntry::Radio {
                id,
                title,
                enabled,
                checked,
                shortcut,
                ..
            } => action(*id, title, *enabled, *checked, shortcut),
            MenuEntry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            MenuEntry::Submenu {
                title,
                enabled,
                entries,
            } => {
                let holder = item(mtm, title, None, "", 0, *enabled);
                let child = new_menu(mtm, title);
                fill_menu(mtm, &child, entries);
                menu.setSubmenu_forItem(Some(&child), &holder);
                menu.addItem(&holder);
            }
        }
    }
}

/// Installs `menus` in the macOS menu bar between the application menu and
/// the Window menu, replacing the ones a previous call installed.
pub(crate) fn install_menu_bar(menus: &[Menu]) -> Result<()> {
    let mtm = main_thread()?;
    let bar = NSApplication::sharedApplication(mtm)
        .mainMenu()
        .ok_or_else(|| Error::new("the application has no menu bar"))?;
    for index in (0..bar.numberOfItems()).rev() {
        if bar
            .itemAtIndex(index)
            .is_some_and(|i| i.tag() == MENU_BAR_TAG)
        {
            bar.removeItemAtIndex(index);
        }
    }
    // After the application menu; the Window menu SDL adds stays last.
    for (at, m) in menus.iter().enumerate() {
        let holder = item(mtm, &m.title, None, "", MENU_BAR_TAG, true);
        let menu = new_menu(mtm, &m.title);
        fill_menu(mtm, &menu, &m.entries);
        bar.setSubmenu_forItem(Some(&menu), &holder);
        bar.insertItem_atIndex(&holder, at as isize + 1);
    }
    Ok(())
}

// Notifications go through `UNUserNotificationCenter`, which asks the user
// once for permission, and need the program to run from an application
// bundle with a bundle identifier.

// 0 until the user answers the permission request, then 1 allowed or 2
// refused. The answer comes on a thread of the framework's.
static PERMISSION: AtomicU8 = AtomicU8::new(0);
static NEXT_NOTIFICATION: AtomicU64 = AtomicU64::new(1);

/// Delivers a notification, asking the user for permission the first time.
pub(crate) fn deliver_notification(title: &str, body: &str) -> Result<()> {
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNUserNotificationCenter,
    };
    if objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .is_none()
    {
        return Err(Error::new(
            "macOS notifications need the program to run from an application bundle with a bundle identifier",
        ));
    }
    if PERMISSION.load(Ordering::Relaxed) == 2 {
        return Err(Error::new(
            "notifications are turned off for this application in System Settings",
        ));
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    if PERMISSION.load(Ordering::Relaxed) == 0 {
        let answered = block2::RcBlock::new(
            |granted: objc2::runtime::Bool, _error: *mut objc2_foundation::NSError| {
                PERMISSION.store(if granted.as_bool() { 1 } else { 2 }, Ordering::Relaxed);
            },
        );
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &answered,
        );
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let id = NSString::from_str(&format!(
        "windowing-notification-{}",
        NEXT_NOTIFICATION.fetch_add(1, Ordering::Relaxed)
    ));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    center.addNotificationRequest_withCompletionHandler(&request, None);
    Ok(())
}

/// Sets the appearance AppKit draws `window` and its title bar in: `code` 1
/// is Aqua, 2 Dark Aqua, and 0 clears it so the window follows the
/// application's (the desktop's) appearance.
pub(crate) fn set_appearance(ns_window: *mut c_void, code: i64) -> Result<()> {
    // SAFETY: SDL's live window.
    let w = unsafe { window(ns_window) }.ok_or_else(|| Error::new("no Cocoa window"))?;
    let appearance = match code {
        0 => None,
        // The `NSAppearanceName...` constants hold their own names.
        _ => {
            let name = if code == 2 {
                "NSAppearanceNameDarkAqua"
            } else {
                "NSAppearanceNameAqua"
            };
            Some(
                NSAppearance::appearanceNamed(&NSString::from_str(name)).ok_or_else(|| {
                    Error::new(format!(
                        "AppKit has no {name} appearance (it needs macOS 10.14 or later)"
                    ))
                })?,
            )
        }
    };
    w.setAppearance(appearance.as_deref());
    Ok(())
}

/// The preferences AppKit reports; macOS has no text scale of its own.
pub(crate) fn preferences() -> crate::system::Preferences {
    let workspace = NSWorkspace::sharedWorkspace();
    let accent = || {
        let srgb =
            NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
        let channel = |c: f64| crate::system::channel(c);
        Some((
            channel(srgb.redComponent())?,
            channel(srgb.greenComponent())?,
            channel(srgb.blueComponent())?,
        ))
    };
    crate::system::Preferences {
        high_contrast: Some(workspace.accessibilityDisplayShouldIncreaseContrast()),
        reduced_motion: Some(workspace.accessibilityDisplayShouldReduceMotion()),
        accent: accent(),
        text_scale: None,
        menu_font_size: Some(NSFont::menuFontOfSize(0.0).pointSize()),
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GosPreferencesObserver"]
    struct PreferencesObserver;

    impl PreferencesObserver {
        #[unsafe(method(changed:))]
        fn changed(&self, _notification: Option<&NSNotification>) {
            crate::system::changed();
        }
    }
);

thread_local! {
    // The observer stays registered for the life of the process.
    static OBSERVER: std::cell::OnceCell<Retained<PreferencesObserver>> = const { std::cell::OnceCell::new() };
}

/// Observes the notifications that change what `preferences` reads.
pub(crate) fn watch_preferences() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if OBSERVER.with(|o| o.get().is_some()) {
        return;
    }
    // SAFETY: NSObject's init on a fresh allocation.
    let made: Retained<PreferencesObserver> =
        unsafe { msg_send![PreferencesObserver::alloc(mtm), init] };
    let observer = OBSERVER.with(|o| o.get_or_init(|| made).clone());
    let observer: &PreferencesObserver = &observer;
    let names = [
        (
            NSWorkspace::sharedWorkspace().notificationCenter(),
            "NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification",
        ),
        (
            NSNotificationCenter::defaultCenter(),
            "NSSystemColorsDidChangeNotification",
        ),
    ];
    for (center, name) in names {
        // SAFETY: `changed:` takes the notification, as the center sends it.
        unsafe {
            center.addObserver_selector_name_object(
                observer,
                sel!(changed:),
                Some(&NSString::from_str(name)),
                None,
            )
        };
    }
}

// Dragging data out: an AppKit dragging session started from the window's
// content view with the mouse event being handled. Text, HTML and other
// MIME types go on one pasteboard item under their pasteboard types; each
// URI of a `text/uri-list` item is its own dragging item, an `NSURL`, as
// Finder expects files. `GosDragSource` answers the operation the program
// offered and queues `Event::DragFinished` when the session ends.

thread_local! {
    static DRAG: Cell<Option<(u64, u32, NSDragOperation)>> = const { Cell::new(None) };
}
static NEXT_DRAG: AtomicU64 = AtomicU64::new(1);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GosDragSource"]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            DRAG.with(Cell::get)
                .map_or(NSDragOperation::None, |(_, _, mask)| mask)
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _at: NSPoint, operation: NSDragOperation) {
            let Some((id, kind, _)) = DRAG.with(|d| d.take()) else {
                return;
            };
            let outcome = if operation.contains(NSDragOperation::Move) {
                Some(DragOperation::Move)
            } else if operation.contains(NSDragOperation::Copy) {
                Some(DragOperation::Copy)
            } else if operation.contains(NSDragOperation::Link) {
                Some(DragOperation::Link)
            } else {
                None
            };
            // AppKit's callback answers nothing; a refused event is a lost outcome.
            let _ = crate::waker::push_user_event(kind, 0, operation_code(outcome), id, 0);
        }
    }
);

fn pasteboard_type(mime: &str) -> &str {
    match mime {
        "text/plain;charset=utf-8" | "text/plain" | "UTF8_STRING" | "TEXT" | "STRING" => {
            "public.utf8-plain-text"
        }
        "text/html" => "public.html",
        "text/rtf" => "public.rtf",
        "image/png" => "public.png",
        other => other,
    }
}

// The mouse event a dragging session starts from: the one AppKit is handling
// when it is a press or drag, else a drag at the pointer.
fn drag_event(mtm: MainThreadMarker, w: &NSWindow) -> Result<Retained<NSEvent>> {
    if let Some(current) = NSApplication::sharedApplication(mtm).currentEvent()
        && matches!(
            current.r#type(),
            NSEventType::LeftMouseDown
                | NSEventType::RightMouseDown
                | NSEventType::LeftMouseDragged
                | NSEventType::RightMouseDragged
                | NSEventType::OtherMouseDown
                | NSEventType::OtherMouseDragged
        )
    {
        return Ok(current);
    }
    let at = w.mouseLocationOutsideOfEventStream();
    let uptime = NSProcessInfo::processInfo().systemUptime();
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDragged,
        at,
        NSEventModifierFlags::empty(),
        uptime,
        w.windowNumber(),
        None,
        0,
        1,
        1.0,
    )
    .ok_or_else(|| Error::new("cannot make the drag's mouse event"))
}

/// Starts an AppKit dragging session of `items` from `window`'s content
/// view; `Event::DragFinished` follows as the event type `finished`.
pub(crate) fn start_drag(
    window_: Window,
    items: Vec<DragItem>,
    operation: DragOperation,
    finished: u32,
) -> Result<DragId> {
    let mtm = main_thread()?;
    if DRAG.with(Cell::get).is_some() {
        return Err(Error::new("a drag is already in progress"));
    }
    let crate::surface::RawHandle::AppKit { ns_window, .. } = window_.raw_handle()? else {
        return Err(Error::new("an AppKit drag needs a Cocoa window"));
    };
    // SAFETY: SDL's live window.
    let w = unsafe { window(ns_window) }.ok_or_else(|| Error::new("no Cocoa window"))?;
    let view: Retained<NSView> = w
        .contentView()
        .ok_or_else(|| Error::new("the window has no content view"))?;
    let event = drag_event(mtm, w)?;
    let at = view.convertPoint_fromView(event.locationInWindow(), None);
    let frame = NSRect::new(
        NSPoint::new(at.x - 16.0, at.y - 16.0),
        NSSize::new(32.0, 32.0),
    );
    // `NSImageNameMultipleDocuments` holds this name.
    let image = NSImage::imageNamed(&NSString::from_str("NSMultipleDocuments"));
    let dragging = |writer: &ProtocolObject<dyn NSPasteboardWriting>| {
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
        // SAFETY: the contents are an image or nothing, as AppKit allows.
        unsafe { item.setDraggingFrame_contents(frame, image.as_deref().map(|i| i as &AnyObject)) };
        item
    };
    let mut dragged = vec![];
    let mut board: Option<Retained<NSPasteboardItem>> = None;
    for item in &items {
        if item.mime == "text/uri-list" {
            for uri in String::from_utf8_lossy(&item.data)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                if let Some(url) = NSURL::URLWithString(&NSString::from_str(uri)) {
                    dragged.push(dragging(ProtocolObject::from_ref(&*url)));
                }
            }
            continue;
        }
        let pasteboard_item = board.get_or_insert_with(NSPasteboardItem::new);
        pasteboard_item.setData_forType(
            &NSData::with_bytes(&item.data),
            &NSString::from_str(pasteboard_type(&item.mime)),
        );
    }
    if let Some(b) = &board {
        dragged.insert(0, dragging(ProtocolObject::from_ref(&**b)));
    }
    if dragged.is_empty() {
        return Err(Error::new(
            "none of the drag's items has a pasteboard representation",
        ));
    }
    let mask = match operation {
        DragOperation::Copy => NSDragOperation::Copy,
        DragOperation::Move => NSDragOperation::Move,
        DragOperation::Link => NSDragOperation::Link,
    };
    let id = NEXT_DRAG.fetch_add(1, Ordering::Relaxed);
    DRAG.with(|d| d.set(Some((id, finished, mask))));
    // SAFETY: NSObject's init on a fresh allocation.
    let source: Retained<DragSource> = unsafe { msg_send![DragSource::alloc(mtm), init] };
    // The session retains its source until the drag ends.
    view.beginDraggingSessionWithItems_event_source(
        &NSArray::from_retained_slice(&dragged),
        &event,
        ProtocolObject::from_ref(&*source),
    );
    Ok(DragId(id))
}
