//! Accessibility providers for the platforms whose screen readers read native
//! objects: UI Automation on Windows (`uia`) and NSAccessibility on macOS
//! (`axmac`). The program describes each window's semantic tree with
//! `update`, as a flat list of elements; the providers answer the screen
//! reader's queries from the latest list on the main thread, and queue what
//! it asks the program to do, which `take_requests` hands over after an
//! `Event::Wake`. On Linux the toolkit speaks AT-SPI over D-Bus itself, so
//! `has_provider` answers false there.
//!
//! Element bounds are in the window's framebuffer pixels, the unit the
//! program draws in; each provider converts them to the screen's.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use parking_lot::Mutex;

use crate::window::{Window, WindowId};
use crate::{Error, Result};

/// Lazily exports a persistent text snapshot when a native client asks for its
/// value. Publishing a semantic tree does not copy a large document.
#[derive(Clone)]
pub struct TextSource {
    pub revision: u64,
    pub read: std::sync::Arc<dyn Fn() -> String + Send + Sync>,
}
impl std::fmt::Debug for TextSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TextSource").field(&self.revision).finish()
    }
}
impl PartialEq for TextSource {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
    }
}

/// One element of a window's semantic tree.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccessElement {
    /// Unique within the window and stable across updates; never 0.
    pub id: u64,
    /// One of the role names `gui::Role::name` answers: "button",
    /// "checkbox", "textfield", "listitem", "tab", ...
    pub role: String,
    pub name: String,
    pub description: String,
    pub value: String,
    #[serde(skip)]
    pub text_source: Option<TextSource>,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub focusable: bool,
    pub focused: bool,
    pub disabled: bool,
    /// -1 not checkable, 0 unchecked, 1 checked, 2 mixed.
    pub checked: i64,
    pub selected: bool,
    /// -1 not expandable, 0 collapsed, 1 expanded.
    pub expanded: i64,
    pub pressed: bool,
    pub read_only: bool,
    pub required: bool,
    pub invalid: bool,
    pub multiline: bool,
    pub modal: bool,
    pub level: i64,
    pub pos_in_set: i64,
    pub set_size: i64,
    pub has_range: bool,
    pub range_min: f64,
    pub range_max: f64,
    pub range_now: f64,
    /// The actions the element takes: "press", "select", "increment", ...
    pub actions: Vec<String>,
    /// "polite" or "assertive" for a live region, else empty.
    pub live: String,
    pub children: Vec<u64>,
}

impl AccessElement {
    pub fn text_value(&self) -> String {
        self.text_source
            .as_ref()
            .map_or_else(|| self.value.clone(), |source| (source.read)())
    }
}

impl Default for AccessElement {
    fn default() -> AccessElement {
        AccessElement {
            id: 0,
            role: "unknown".into(),
            name: String::new(),
            description: String::new(),
            value: String::new(),
            text_source: None,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            focusable: false,
            focused: false,
            disabled: false,
            checked: -1,
            selected: false,
            expanded: -1,
            pressed: false,
            read_only: false,
            required: false,
            invalid: false,
            multiline: false,
            modal: false,
            level: 0,
            pos_in_set: 0,
            set_size: 0,
            has_range: false,
            range_min: 0.0,
            range_max: 0.0,
            range_now: 0.0,
            actions: Vec::new(),
            live: String::new(),
            children: Vec::new(),
        }
    }
}

/// What a screen reader asked the program to do to an element: an action
/// the element listed, "focus", "set-text" with `value`, or "set-value"
/// with a number in `value`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccessRequest {
    pub window: WindowId,
    pub id: u64,
    pub action: String,
    pub value: String,
}

/// A window's latest tree: its elements by id, the root's id, and which
/// element has focus.
#[derive(Debug, Clone, Default)]
#[cfg_attr(
    not(any(target_os = "windows", target_os = "macos", test)),
    expect(
        dead_code,
        reason = "only the Windows and macOS providers read a stored tree"
    )
)]
pub(crate) struct Tree {
    pub(crate) elements: HashMap<u64, AccessElement>,
    pub(crate) parents: HashMap<u64, u64>,
    pub(crate) root: u64,
    pub(crate) focus: u64,
}

impl Tree {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) fn get(&self, id: u64) -> Option<&AccessElement> {
        self.elements.get(&id)
    }

    /// The id of `id`'s parent, 0 for the root.
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn parent(&self, id: u64) -> u64 {
        self.parents.get(&id).copied().unwrap_or(0)
    }

    /// The sibling `step` places from `id` (-1 before, 1 after), 0 at an end.
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn sibling(&self, id: u64, step: i64) -> u64 {
        let Some(parent) = self.elements.get(&self.parent(id)) else {
            return 0;
        };
        let Some(at) = parent.children.iter().position(|c| *c == id) else {
            return 0;
        };
        let next = at as i64 + step;
        if next < 0 || next >= parent.children.len() as i64 {
            return 0;
        }
        parent.children[next as usize]
    }

    /// The deepest element whose bounds hold (`x`, `y`), the last-drawn child
    /// first; the root when no other does.
    #[cfg(any(target_os = "windows", target_os = "macos", test))]
    pub(crate) fn hit(&self, x: i64, y: i64) -> u64 {
        let mut at = self.root;
        loop {
            let Some(e) = self.elements.get(&at) else {
                return at;
            };
            let found = e.children.iter().rev().copied().find(|c| {
                self.elements.get(c).is_some_and(|child| {
                    x >= child.x
                        && y >= child.y
                        && x < child.x + child.width
                        && y < child.y + child.height
                })
            });
            match found {
                Some(c) => at = c,
                None => return at,
            }
        }
    }
}

// Trees by window id; requests a screen reader made, oldest first.
static TREES: Mutex<Option<HashMap<u64, Tree>>> = Mutex::new(None);
static REQUESTS: Mutex<Vec<AccessRequest>> = Mutex::new(Vec::new());
// The event type of `Event::Wake`, which a queued request posts.
static WAKE_EVENT_TYPE: AtomicU32 = AtomicU32::new(0);
// Whether a screen reader or other client has asked for a tree.
static QUERIED: AtomicBool = AtomicBool::new(false);

/// Records the event type a queued request posts to wake the program.
pub(crate) fn set_wake_event(wake_type: u32) {
    WAKE_EVENT_TYPE.store(wake_type, Ordering::Release);
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn tree(window: u64) -> Option<Tree> {
    TREES.lock().as_ref().and_then(|t| t.get(&window).cloned())
}

/// Records that a client asked for a tree, waking the program the first time
/// so it starts describing its windows.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn queried() {
    if !QUERIED.swap(true, Ordering::AcqRel) {
        wake();
    }
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn wake() {
    // A refused wake leaves the request queued for the next event.
    let _ = crate::waker::push_user_event(WAKE_EVENT_TYPE.load(Ordering::Acquire), 0, 0, 0, 0);
}

/// Queues what a screen reader asked of element `id` in `window`.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) fn request(window: u64, id: u64, action: &str, value: &str) {
    {
        let mut requests = REQUESTS.lock();
        if requests.len() >= 256 {
            return;
        }
        requests.push(AccessRequest {
            window: WindowId(window as u32),
            id,
            action: action.to_string(),
            value: value.to_string(),
        });
    }
    wake();
}

pub(crate) fn forget(window: u64) {
    if let Some(trees) = TREES.lock().as_mut() {
        trees.remove(&window);
    }
}

/// Whether this platform has a provider here (Windows UI Automation, macOS
/// NSAccessibility).
pub fn has_provider() -> bool {
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        matches!(crate::video_driver().as_str(), "windows" | "cocoa")
    } else {
        false
    }
}

/// Whether a screen reader or other client is reading the program's
/// windows, so trees are worth describing.
pub fn listening() -> bool {
    QUERIED.load(Ordering::Acquire) || clients_listening()
}

fn clients_listening() -> bool {
    #[cfg(target_os = "windows")]
    return crate::uia::clients_listening();
    #[cfg(target_os = "macos")]
    return crate::axmac::voice_over_running();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    false
}

/// Makes `elements` the semantic tree of `window`. The first element is the
/// window itself (its name is the window's accessible name); every other
/// element is reached from it through `children`. Screen readers hear what
/// changed.
pub fn update(window: Window, elements: Vec<AccessElement>) -> Result<()> {
    crate::support::require_main_thread("access::update");
    let Some(root) = elements.first().map(|e| e.id) else {
        return Err(Error::new(
            "an accessibility tree needs the window's own element",
        ));
    };
    let mut by_id = HashMap::new();
    let mut parents = HashMap::new();
    let mut focus = 0;
    for e in elements {
        if e.id == 0 {
            return Err(Error::new("an accessibility element's id is 0"));
        }
        if by_id.contains_key(&e.id) {
            return Err(Error::new(format!(
                "two accessibility elements share the id {}",
                e.id
            )));
        }
        for c in &e.children {
            parents.insert(*c, e.id);
        }
        if e.focused {
            focus = e.id;
        }
        by_id.insert(e.id, e);
    }
    let key = u64::from(window.id().0);
    let next = Tree {
        elements: by_id,
        parents,
        root,
        focus,
    };
    let previous = TREES
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(key, next.clone());
    announce_changes(window, previous, &next)
}

/// Moves the screen reader's focus to element `id` of `window`, as the
/// program's focus moved there.
pub fn focus(window: Window, id: u64) -> Result<()> {
    crate::support::require_main_thread("access::focus");
    let key = u64::from(window.id().0);
    {
        let mut trees = TREES.lock();
        let Some(t) = trees.as_mut().and_then(|t| t.get_mut(&key)) else {
            return Ok(());
        };
        t.focus = id;
    }
    focus_changed(window, id)
}

/// Asks the screen reader to speak `text`, ahead of what it is saying when
/// `assertive`.
pub fn announce(window: Window, text: &str, assertive: bool) -> Result<()> {
    crate::support::require_main_thread("access::announce");
    platform_announce(window, text, assertive)
}

/// The requests screen readers made since the last call, oldest first.
pub fn take_requests() -> Vec<AccessRequest> {
    std::mem::take(&mut *REQUESTS.lock())
}

#[allow(
    unused_variables,
    reason = "the parameters feed only the Windows and macOS providers"
)]
fn announce_changes(window: Window, previous: Option<Tree>, next: &Tree) -> Result<()> {
    #[cfg(target_os = "windows")]
    return crate::uia::tree_changed(window, previous, next);
    #[cfg(target_os = "macos")]
    return crate::axmac::tree_changed(window, previous, next);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    Ok(())
}

#[allow(
    unused_variables,
    reason = "the parameters feed only the Windows and macOS providers"
)]
fn focus_changed(window: Window, id: u64) -> Result<()> {
    #[cfg(target_os = "windows")]
    return crate::uia::focus_changed(window, id);
    #[cfg(target_os = "macos")]
    return crate::axmac::focus_changed(window, id);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    Ok(())
}

#[allow(
    unused_variables,
    reason = "the parameters feed only the Windows and macOS providers"
)]
fn platform_announce(window: Window, text: &str, assertive: bool) -> Result<()> {
    #[cfg(target_os = "windows")]
    return crate::uia::announce(window, text, assertive);
    #[cfg(target_os = "macos")]
    return crate::axmac::announce(window, text, assertive);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    Err(Error::new(
        "announcements go through the toolkit's AT-SPI bridge here",
    ))
}

/// Connects a new window to the platform's provider.
#[allow(
    unused_variables,
    reason = "the window feeds only the Windows and macOS providers"
)]
pub(crate) fn attach(window: &Window) {
    #[cfg(target_os = "windows")]
    crate::uia::attach(*window);
    #[cfg(target_os = "macos")]
    crate::axmac::attach(*window);
}

/// Follows a window's move: macOS elements carry screen frames.
#[allow(unused_variables, reason = "the window feeds only the macOS provider")]
pub(crate) fn moved(window: u64) {
    #[cfg(target_os = "macos")]
    crate::axmac::window_moved(window);
}

/// Disconnects a window about to close from the platform's provider and
/// drops its tree.
pub(crate) fn detach(window: &Window) {
    #[cfg(target_os = "windows")]
    crate::uia::detach(*window);
    #[cfg(target_os = "macos")]
    crate::axmac::detach(*window);
    forget(u64::from(window.id().0));
}

#[cfg(test)]
mod tests {
    use super::{AccessElement, Tree, request, take_requests};

    fn sample() -> Tree {
        let elements = vec![
            AccessElement {
                id: 10,
                role: "window".into(),
                width: 200,
                height: 100,
                children: vec![11, 12],
                ..AccessElement::default()
            },
            AccessElement {
                id: 11,
                role: "button".into(),
                name: "Back".into(),
                x: 0,
                y: 0,
                width: 100,
                height: 100,
                actions: vec!["press".into()],
                ..AccessElement::default()
            },
            AccessElement {
                id: 12,
                role: "group".into(),
                x: 100,
                y: 0,
                width: 100,
                height: 100,
                children: vec![13],
                ..AccessElement::default()
            },
            AccessElement {
                id: 13,
                role: "checkbox".into(),
                name: "Wrap".into(),
                x: 120,
                y: 20,
                width: 40,
                height: 20,
                checked: 0,
                focused: true,
                ..AccessElement::default()
            },
        ];
        let mut t = Tree {
            root: 10,
            ..Tree::default()
        };
        for e in elements {
            for c in &e.children {
                t.parents.insert(*c, e.id);
            }
            if e.focused {
                t.focus = e.id;
            }
            t.elements.insert(e.id, e);
        }
        t
    }

    #[test]
    fn a_tree_answers_parents_siblings_and_hits() {
        let t = sample();
        assert_eq!(t.focus, 13);
        assert_eq!(t.parent(13), 12);
        assert_eq!(t.parent(10), 0);
        assert_eq!(t.sibling(11, 1), 12);
        assert_eq!(t.sibling(11, -1), 0);
        assert_eq!(t.sibling(12, 1), 0);
        assert_eq!(t.hit(130, 25), 13);
        assert_eq!(t.hit(130, 80), 12);
        assert_eq!(t.hit(10, 10), 11);
        assert_eq!(t.hit(500, 500), 10);
    }

    #[test]
    fn requests_queue_in_order_until_taken() {
        let _ = take_requests();
        request(3, 11, "press", "");
        request(3, 13, "set-value", "7");
        let taken = take_requests();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].id, 11);
        assert_eq!(taken[1].action, "set-value");
        assert_eq!(taken[1].value, "7");
        assert!(take_requests().is_empty(), "taking empties the queue");
    }
}
