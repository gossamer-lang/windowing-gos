// The NSAccessibility provider: each element of a window's tree is a
// `GosAccessibilityElement`, a subclass of `NSAccessibilityElement` whose
// getters read the latest tree `access::update` gave and whose actions
// queue requests through `access::request`. The window's content view
// (SDL's) gains `accessibilityChildren`, `accessibilityFocusedUIElement` and
// `accessibilityHitTest:`, so VoiceOver reaches the elements from the window.
// AppKit calls all of these on the main thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObjectProtocol, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityAnnouncementKey,
    NSAccessibilityAnnouncementRequestedNotification, NSAccessibilityElement,
    NSAccessibilityFocusedUIElementChangedNotification, NSAccessibilityLayoutChangedNotification,
    NSAccessibilityPostNotification, NSAccessibilityPostNotificationWithUserInfo,
    NSAccessibilityPriorityKey, NSAccessibilitySelectedChildrenChangedNotification,
    NSAccessibilityTitleChangedNotification, NSAccessibilityUIElementDestroyedNotification,
    NSAccessibilityValueChangedNotification, NSApplication, NSView, NSWorkspace,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};

use crate::access::{self, AccessElement, Tree};
use crate::window::Window;
use crate::{Error, Result};

// `NSAccessibilityPriorityMedium` and `NSAccessibilityPriorityHigh`.
const PRIORITY_MEDIUM: isize = 50;
const PRIORITY_HIGH: isize = 90;

pub(crate) struct Ids {
    window: u64,
    id: u64,
}

define_class!(
    #[unsafe(super(NSAccessibilityElement))]
    #[name = "GosAccessibilityElement"]
    #[ivars = Ids]
    struct Element;

    impl Element {
        #[unsafe(method_id(accessibilityRole))]
        fn role(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(self.data().map_or("AXUnknown", |(_, e)| role_of(&e).0)))
        }

        #[unsafe(method_id(accessibilitySubrole))]
        fn subrole(&self) -> Option<Retained<NSString>> {
            self.data().and_then(|(_, e)| text(role_of(&e).1))
        }

        #[unsafe(method_id(accessibilityLabel))]
        fn label(&self) -> Option<Retained<NSString>> {
            self.data().and_then(|(_, e)| text(&e.name))
        }

        #[unsafe(method_id(accessibilityHelp))]
        fn help(&self) -> Option<Retained<NSString>> {
            self.data().and_then(|(_, e)| text(&e.description))
        }

        #[unsafe(method_id(accessibilityValue))]
        fn value(&self) -> Option<Retained<AnyObject>> {
            self.value_object()
        }

        #[unsafe(method_id(accessibilityMinValue))]
        fn min_value(&self) -> Option<Retained<NSNumber>> {
            self.data().filter(|(_, e)| e.has_range).map(|(_, e)| NSNumber::new_f64(e.range_min))
        }

        #[unsafe(method_id(accessibilityMaxValue))]
        fn max_value(&self) -> Option<Retained<NSNumber>> {
            self.data().filter(|(_, e)| e.has_range).map(|(_, e)| NSNumber::new_f64(e.range_max))
        }

        #[unsafe(method_id(accessibilityChildren))]
        fn children(&self) -> Option<Retained<NSArray<Element>>> {
            self.data().map(|(_, e)| objects(self.ivars().window, &e.children))
        }

        #[unsafe(method_id(accessibilityParent))]
        fn parent(&self) -> Option<Retained<AnyObject>> {
            self.parent_object()
        }

        #[unsafe(method_id(accessibilityWindow))]
        fn window(&self) -> Option<Retained<AnyObject>> {
            view_of(self.ivars().window).and_then(|v| v.window()).map(any)
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_element(&self) -> bool {
            self.data().is_some()
        }

        #[unsafe(method(isAccessibilityEnabled))]
        fn enabled(&self) -> bool {
            self.data().is_some_and(|(_, e)| !e.disabled)
        }

        #[unsafe(method(isAccessibilityFocused))]
        fn focused(&self) -> bool {
            self.data().is_some_and(|(t, e)| t.focus == e.id || e.focused)
        }

        #[unsafe(method(isAccessibilitySelected))]
        fn selected(&self) -> bool {
            self.data().is_some_and(|(_, e)| e.selected)
        }

        #[unsafe(method(isAccessibilityExpanded))]
        fn expanded(&self) -> bool {
            self.data().is_some_and(|(_, e)| e.expanded == 1)
        }

        #[unsafe(method(isAccessibilityRequired))]
        fn required(&self) -> bool {
            self.data().is_some_and(|(_, e)| e.required)
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn press(&self) -> bool {
            self.first_action(&["press", "activate", "toggle", "open", "select"])
        }

        #[unsafe(method(accessibilityPerformIncrement))]
        fn increment(&self) -> bool {
            self.step("increment", 1.0)
        }

        #[unsafe(method(accessibilityPerformDecrement))]
        fn decrement(&self) -> bool {
            self.step("decrement", -1.0)
        }

        #[unsafe(method(accessibilityPerformShowMenu))]
        fn show_menu(&self) -> bool {
            self.first_action(&["menu", "open"])
        }

        #[unsafe(method(setAccessibilityValue:))]
        fn set_value(&self, value: Option<&AnyObject>) {
            if let Some(v) = value {
                self.write_value(v);
            }
        }

        #[unsafe(method(setAccessibilityFocused:))]
        fn set_focused(&self, focused: bool) {
            if focused {
                self.act("focus", "");
            }
        }

        #[unsafe(method(setAccessibilitySelected:))]
        fn set_selected(&self, selected: bool) {
            if selected {
                self.first_action(&["select", "press"]);
            }
        }

        #[unsafe(method(setAccessibilityExpanded:))]
        fn set_expanded(&self, expanded: bool) {
            if self.data().is_some_and(|(_, e)| e.expanded >= 0 && e.expanded != i64::from(expanded)) {
                self.first_action(&[if expanded { "expand" } else { "collapse" }, "toggle-expanded", "press"]);
            }
        }

        #[unsafe(method(isAccessibilitySelectorAllowed:))]
        fn selector_allowed(&self, asked: Sel) -> bool {
            self.data().is_some_and(|(_, e)| allowed(&e, asked.name().to_str().unwrap_or_default()))
        }
    }

    unsafe impl NSObjectProtocol for Element {}
);

// Whether VoiceOver may call `selector` on `e`.
fn allowed(e: &AccessElement, selector: &str) -> bool {
    let has = |a: &str| e.actions.iter().any(|x| x == a);
    let typed = matches!(
        e.role.as_str(),
        "textfield" | "passwordfield" | "searchfield" | "textarea" | "combobox"
    );
    match selector {
        "accessibilityPerformPress" => {
            !e.disabled
                && ["press", "activate", "toggle", "open", "select"]
                    .iter()
                    .any(|a| has(a))
        }
        "accessibilityPerformIncrement" => {
            !e.disabled && (has("increment") || (e.has_range && !e.read_only))
        }
        "accessibilityPerformDecrement" => {
            !e.disabled && (has("decrement") || (e.has_range && !e.read_only))
        }
        "accessibilityPerformShowMenu" => !e.disabled && (has("menu") || has("open")),
        "setAccessibilityValue:" => !e.disabled && !e.read_only && (typed || e.has_range),
        "setAccessibilityFocused:" => !e.disabled && e.focusable,
        "setAccessibilitySelected:" => {
            !e.disabled
                && (has("select")
                    || matches!(e.role.as_str(), "tab" | "listitem" | "treeitem" | "radio"))
        }
        "setAccessibilityExpanded:" => !e.disabled && e.expanded >= 0,
        "accessibilityMinValue" | "accessibilityMaxValue" => e.has_range,
        "isAccessibilityExpanded" => e.expanded >= 0,
        _ => true,
    }
}

fn any<T: Message>(o: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an `AnyObject`.
    unsafe { Retained::cast_unchecked(o) }
}

impl Element {
    fn value_object(&self) -> Option<Retained<AnyObject>> {
        let (_, e) = self.data()?;
        if e.has_range {
            return Some(any(NSNumber::new_f64(e.range_now)));
        }
        let int = |n: i64| Some(any(NSNumber::new_isize(n as isize)));
        match e.role.as_str() {
            "checkbox" | "switch" | "menuitemcheckbox" => int(e.checked.max(0)),
            "togglebutton" => int(if e.checked >= 0 {
                e.checked
            } else {
                i64::from(e.pressed)
            }),
            "radio" | "tab" | "menuitemradio" => int(i64::from(e.selected || e.checked == 1)),
            "passwordfield" => Some(any(NSString::new())),
            _ => text(&e.text_value()).map(any),
        }
    }

    fn parent_object(&self) -> Option<Retained<AnyObject>> {
        let Ids { window, id } = *self.ivars();
        let t = access::tree(window)?;
        let parent = t.parent(id);
        if parent == 0 || parent == t.root {
            return view_of(window).map(any);
        }
        object_for(window, parent).map(any)
    }

    fn write_value(&self, v: &AnyObject) {
        let Some((_, e)) = self.data() else { return };
        if e.read_only {
            return;
        }
        let as_text = v.downcast_ref::<NSString>().map(|s| s.to_string());
        if e.has_range {
            let wanted = match &as_text {
                Some(t) => t.parse().unwrap_or(e.range_now),
                None => v
                    .downcast_ref::<NSNumber>()
                    .map_or(e.range_now, |n| n.as_f64()),
            };
            if (e.range_min..=e.range_max).contains(&wanted) {
                self.act("set-value", &wanted.to_string());
            }
        } else if let Some(t) = as_text {
            self.act("set-text", &t);
        }
    }

    fn data(&self) -> Option<(Tree, AccessElement)> {
        let Ids { window, id } = *self.ivars();
        let t = access::tree(window)?;
        let e = t.get(id)?.clone();
        Some((t, e))
    }

    // Queues `action` for the element, when it is enabled.
    fn act(&self, action: &str, value: &str) -> bool {
        let Some((_, e)) = self.data() else {
            return false;
        };
        if e.disabled {
            return false;
        }
        access::request(self.ivars().window, e.id, action, value);
        true
    }

    fn first_action(&self, wanted: &[&str]) -> bool {
        let Some((_, e)) = self.data() else {
            return false;
        };
        wanted
            .iter()
            .find(|w| e.actions.iter().any(|a| a == *w))
            .is_some_and(|w| self.act(w, ""))
    }

    fn step(&self, named: &str, direction: f64) -> bool {
        let Some((_, e)) = self.data() else {
            return false;
        };
        if e.actions.iter().any(|a| a == named) {
            return self.act(named, "");
        }
        if !e.has_range || e.read_only {
            return false;
        }
        // The program does not report its step, so a tenth of the range stands in.
        let next = (e.range_now + direction * (e.range_max - e.range_min) / 10.0)
            .clamp(e.range_min, e.range_max);
        self.act("set-value", &next.to_string())
    }
}

fn text(s: &str) -> Option<Retained<NSString>> {
    (!s.is_empty()).then(|| NSString::from_str(s))
}

fn role_of(e: &AccessElement) -> (&'static str, &'static str) {
    match e.role.as_str() {
        "dialog" => ("AXGroup", "AXApplicationDialog"),
        "alertdialog" => ("AXGroup", "AXApplicationAlertDialog"),
        "alert" => ("AXGroup", "AXApplicationAlert"),
        "radiogroup" => ("AXRadioGroup", ""),
        "heading" => ("AXHeading", ""),
        "label" | "paragraph" | "listitem" => ("AXStaticText", ""),
        "link" => ("AXLink", ""),
        "image" | "canvas" => ("AXImage", ""),
        "separator" | "splitter" => ("AXSplitter", ""),
        "button" => ("AXButton", ""),
        "togglebutton" => ("AXCheckBox", "AXToggle"),
        "checkbox" => ("AXCheckBox", ""),
        "switch" => ("AXCheckBox", "AXSwitch"),
        "radio" => ("AXRadioButton", ""),
        "textfield" => ("AXTextField", ""),
        "passwordfield" => ("AXTextField", "AXSecureTextField"),
        "searchfield" => ("AXTextField", "AXSearchField"),
        "textarea" | "terminal" => ("AXTextArea", ""),
        "combobox" => ("AXComboBox", ""),
        "listbox" => ("AXList", ""),
        "menu" => ("AXMenu", ""),
        "menubar" => ("AXMenuBar", ""),
        "menuitem" | "menuitemcheckbox" | "menuitemradio" => ("AXMenuItem", ""),
        "toolbar" => ("AXToolbar", ""),
        "slider" => ("AXSlider", ""),
        "spinbutton" => ("AXIncrementor", ""),
        "progressbar" => ("AXProgressIndicator", ""),
        "scrollarea" => ("AXScrollArea", ""),
        "scrollbar" => ("AXScrollBar", ""),
        "tablist" => ("AXTabGroup", ""),
        "tab" => ("AXRadioButton", "AXTabButton"),
        "table" | "grid" => ("AXTable", ""),
        "row" => ("AXRow", ""),
        "cell" | "columnheader" => ("AXCell", ""),
        "tree" => ("AXOutline", ""),
        "treeitem" => ("AXRow", "AXOutlineRow"),
        "colorwell" => ("AXColorWell", ""),
        "document" => ("AXGroup", "AXDocument"),
        "group" | "pane" | "tabpanel" | "statusbar" | "status" | "tooltip" | "calendar"
        | "window" => ("AXGroup", ""),
        _ => ("AXUnknown", ""),
    }
}

thread_local! {
    // The element objects by (window, id), and the content views by window.
    static OBJECTS: RefCell<HashMap<(u64, u64), Retained<Element>>> = RefCell::new(HashMap::new());
    static VIEWS: RefCell<HashMap<u64, Retained<NSView>>> = RefCell::new(HashMap::new());
    static VIEW_CLASSES: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

// The element object for element `id` of `window`, made the first time.
fn object_for(window: u64, id: u64) -> Option<Retained<Element>> {
    if let Some(o) = OBJECTS.with_borrow(|m| m.get(&(window, id)).cloned()) {
        return Some(o);
    }
    access::tree(window)?.get(id)?;
    let made = Element::alloc().set_ivars(Ids { window, id });
    // SAFETY: NSAccessibilityElement's designated init.
    let made: Retained<Element> = unsafe { msg_send![super(made), init] };
    OBJECTS.with_borrow_mut(|m| m.insert((window, id), made.clone()));
    Some(made)
}

fn objects(window: u64, ids: &[u64]) -> Retained<NSArray<Element>> {
    let found: Vec<Retained<Element>> = ids
        .iter()
        .filter_map(|&id| object_for(window, id))
        .collect();
    NSArray::from_retained_slice(&found)
}

fn view_of(window: u64) -> Option<Retained<NSView>> {
    VIEWS.with_borrow(|m| m.get(&window).cloned())
}

// The content view's window id.
fn view_window(view: &NSView) -> Option<u64> {
    let w = view.window()?;
    crate::window::sdl_id_of_cocoa_window(Retained::as_ptr(&w) as *mut c_void).map(u64::from)
}

// The window's size in points and its pixel density.
fn window_metrics(window: u64) -> (f64, f64) {
    // SAFETY: SDL looks the window up by id.
    unsafe {
        let h = sdl3_sys::video::SDL_GetWindowFromID(sdl3_sys::video::SDL_WindowID(window as u32));
        if h.is_null() {
            return (0.0, 1.0);
        }
        let (mut w, mut ht) = (0, 0);
        let _ = sdl3_sys::video::SDL_GetWindowSize(h, &mut w, &mut ht);
        let d = f64::from(sdl3_sys::video::SDL_GetWindowPixelDensity(h));
        (f64::from(ht), if d > 0.0 { d } else { 1.0 })
    }
}

// A point in the window's framebuffer pixels, on screen; AppKit views count
// up from the bottom unless flipped.
fn screen_point(window: u64, view: &NSView, x: f64, y: f64) -> Option<NSPoint> {
    let (height, d) = window_metrics(window);
    let local = NSPoint::new(
        x / d,
        if view.isFlipped() {
            y / d
        } else {
            height - y / d
        },
    );
    let w = view.window()?;
    Some(w.convertPointToScreen(view.convertPoint_toView(local, None)))
}

extern "C-unwind" fn view_children(this: &NSView, _cmd: Sel) -> *mut NSArray<Element> {
    access::queried();
    let found = view_window(this).and_then(|w| {
        let t = access::tree(w)?;
        let root = t.get(t.root)?;
        Some(objects(w, &root.children))
    });
    found.map_or(std::ptr::null_mut(), Retained::autorelease_return)
}

extern "C-unwind" fn view_focused(this: &NSView, _cmd: Sel) -> *mut AnyObject {
    access::queried();
    let focused = view_window(this).and_then(|w| {
        let t = access::tree(w)?;
        (t.focus != 0 && t.focus != t.root && t.get(t.focus).is_some())
            .then(|| object_for(w, t.focus))
            .flatten()
    });
    match focused {
        Some(o) => Retained::autorelease_return(any(o)),
        None => this as *const NSView as *mut AnyObject,
    }
}

extern "C-unwind" fn view_hit_test(this: &NSView, _cmd: Sel, at: NSPoint) -> *mut AnyObject {
    access::queried();
    let me = this as *const NSView as *mut AnyObject;
    let (Some(window), Some(ns_window)) = (view_window(this), this.window()) else {
        return me;
    };
    let Some(t) = access::tree(window) else {
        return me;
    };
    let local = this.convertPoint_fromView(ns_window.convertPointFromScreen(at), None);
    let (height, d) = window_metrics(window);
    let py = if this.isFlipped() {
        local.y * d
    } else {
        (height - local.y) * d
    };
    let hit = t.hit((local.x * d) as i64, py as i64);
    if hit == t.root {
        return me;
    }
    object_for(window, hit).map_or(me, |o| Retained::autorelease_return(any(o)))
}

fn add_method(class: &AnyClass, name: Sel, imp: Imp, types: &std::ffi::CStr) {
    // SAFETY: `imp` has the argument and return types `types` spells.
    unsafe {
        objc2::ffi::class_addMethod(
            class as *const AnyClass as *mut AnyClass,
            name,
            imp,
            types.as_ptr(),
        );
    }
}

/// Gives `window`'s content view the methods that lead VoiceOver to its
/// elements.
pub(crate) fn attach(window: Window) {
    let Ok(crate::surface::RawHandle::AppKit { ns_view, .. }) = window.raw_handle() else {
        return;
    };
    // SAFETY: SDL's live content view, or null.
    let Some(view) = (unsafe { Retained::retain(ns_view.cast::<NSView>()) }) else {
        return;
    };
    let class = view.class();
    VIEWS.with_borrow_mut(|m| m.insert(u64::from(window.id().0), view.clone()));
    if !VIEW_CLASSES.with_borrow_mut(|c| c.insert(class as *const AnyClass as usize)) {
        return;
    }
    // SDL's view class defines none of these, so each overrides NSView's.
    // SAFETY: each function matches its selector's signature.
    unsafe {
        add_method(
            class,
            sel!(accessibilityChildren),
            std::mem::transmute::<extern "C-unwind" fn(&NSView, Sel) -> *mut NSArray<Element>, Imp>(
                view_children,
            ),
            c"@@:",
        );
        add_method(
            class,
            sel!(accessibilityFocusedUIElement),
            std::mem::transmute::<extern "C-unwind" fn(&NSView, Sel) -> *mut AnyObject, Imp>(
                view_focused,
            ),
            c"@@:",
        );
        add_method(
            class,
            sel!(accessibilityHitTest:),
            std::mem::transmute::<extern "C-unwind" fn(&NSView, Sel, NSPoint) -> *mut AnyObject, Imp>(
                view_hit_test,
            ),
            c"@@:{CGPoint=dd}",
        );
    }
}

/// Releases `window`'s element objects; VoiceOver hears they are gone.
pub(crate) fn detach(window: Window) {
    let w = u64::from(window.id().0);
    let gone: Vec<Retained<Element>> = OBJECTS.with_borrow_mut(|m| {
        let keys: Vec<(u64, u64)> = m.keys().filter(|k| k.0 == w).copied().collect();
        keys.iter().filter_map(|k| m.remove(k)).collect()
    });
    for o in gone {
        // SAFETY: posting on a live element.
        unsafe {
            NSAccessibilityPostNotification(&o, NSAccessibilityUIElementDestroyedNotification)
        };
    }
    VIEWS.with_borrow_mut(|m| m.remove(&w));
}

fn release(window: u64, id: u64) {
    if let Some(o) = OBJECTS.with_borrow_mut(|m| m.remove(&(window, id))) {
        // SAFETY: posting on a live element.
        unsafe {
            NSAccessibilityPostNotification(&o, NSAccessibilityUIElementDestroyedNotification)
        };
    }
}

/// Whether VoiceOver is running.
pub(crate) fn voice_over_running() -> bool {
    NSWorkspace::sharedWorkspace().isVoiceOverEnabled()
}

fn post(object: &AnyObject, notification: &NSString) {
    // SAFETY: posting on a live object.
    unsafe { NSAccessibilityPostNotification(object, notification) };
}

// Sets the screen frame of every element of `t` in `window`.
fn place(window: u64, t: &Tree) {
    let Some(view) = view_of(window) else { return };
    for (&id, e) in &t.elements {
        if id == t.root {
            continue;
        }
        let (Some(o), Some(a), Some(b)) = (
            object_for(window, id),
            screen_point(window, &view, e.x as f64, e.y as f64),
            screen_point(
                window,
                &view,
                (e.x + e.width) as f64,
                (e.y + e.height) as f64,
            ),
        ) else {
            continue;
        };
        let frame = NSRect::new(
            NSPoint::new(a.x.min(b.x), a.y.min(b.y)),
            NSSize::new((b.x - a.x).abs(), (b.y - a.y).abs()),
        );
        o.setAccessibilityFrame(frame);
    }
}

/// Moves every element's frame with the window.
pub(crate) fn window_moved(window: u64) {
    if let Some(t) = access::tree(window) {
        place(window, &t);
    }
}

/// Places the new tree's elements and tells VoiceOver what changed.
pub(crate) fn tree_changed(window: Window, previous: Option<Tree>, next: &Tree) -> Result<()> {
    let w = u64::from(window.id().0);
    // SAFETY: the notification names are AppKit's constant strings.
    let (title, value, layout, selected) = unsafe {
        (
            NSAccessibilityTitleChangedNotification,
            NSAccessibilityValueChangedNotification,
            NSAccessibilityLayoutChangedNotification,
            NSAccessibilitySelectedChildrenChangedNotification,
        )
    };
    if let Some(before) = &previous {
        for &id in before.elements.keys() {
            if !next.elements.contains_key(&id) {
                release(w, id);
            }
        }
        for (&id, e) in &next.elements {
            let (Some(old), Some(o)) = (before.get(id), object_for(w, id)) else {
                continue;
            };
            if old.name != e.name {
                post(&o, title);
            }
            if old.value != e.value
                || old.checked != e.checked
                || old.range_now != e.range_now
                || old.selected != e.selected
                || old.expanded != e.expanded
            {
                post(&o, value);
            }
            if old.children != e.children {
                post(&o, layout);
            }
            if e.selected
                && !old.selected
                && let Some(parent) = object_for(w, next.parent(id))
            {
                post(&parent, selected);
            }
        }
    }
    place(w, next);
    if let Some(view) = view_of(w)
        && previous.as_ref().map_or(0, |p| p.elements.len()) != next.elements.len()
    {
        post(&view, layout);
    }
    if next.focus != 0 && previous.as_ref().map_or(0, |p| p.focus) != next.focus {
        focus_changed(window, next.focus)?;
    }
    Ok(())
}

/// Tells VoiceOver element `id` of `window` took the focus.
pub(crate) fn focus_changed(window: Window, id: u64) -> Result<()> {
    if let Some(o) = object_for(u64::from(window.id().0), id) {
        // SAFETY: the notification name is AppKit's constant string.
        post(&o, unsafe {
            NSAccessibilityFocusedUIElementChangedNotification
        });
    }
    Ok(())
}

/// Asks VoiceOver to speak `text`, at high priority when `assertive`.
pub(crate) fn announce(window: Window, text_: &str, assertive: bool) -> Result<()> {
    let view = view_of(u64::from(window.id().0))
        .ok_or_else(|| Error::new("the window has no content view"))?;
    let spoken = any(NSString::from_str(text_));
    let priority = any(NSNumber::new_isize(if assertive {
        PRIORITY_HIGH
    } else {
        PRIORITY_MEDIUM
    }));
    // SAFETY: the keys and notification are AppKit's constant strings.
    unsafe {
        let info = NSDictionary::from_retained_objects(
            &[NSAccessibilityAnnouncementKey, NSAccessibilityPriorityKey],
            &[spoken, priority],
        );
        let target = match MainThreadMarker::new() {
            Some(mtm) => any(NSApplication::sharedApplication(mtm)),
            None => any(view),
        };
        NSAccessibilityPostNotificationWithUserInfo(
            &target,
            NSAccessibilityAnnouncementRequestedNotification,
            Some(&info),
        );
    }
    Ok(())
}
