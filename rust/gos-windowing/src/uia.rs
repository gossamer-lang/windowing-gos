// The UI Automation provider: each window answers `WM_GETOBJECT` with a
// fragment root for its semantic tree, and every element is a COM object
// implementing the provider interfaces and the control patterns its state
// calls for. Providers ask for COM threading, so UI Automation calls them on
// the main thread while it pumps messages; they read the latest tree
// `access::update` gave and queue the program's actions through
// `access::request`. Element 0 is the window's root, whatever id the
// program's root carries.

use windows::Win32::Foundation::{E_INVALIDARG, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Ole::{OleInitialize, SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, WM_GETOBJECT, WM_NCDESTROY};
use windows::core::{
    BOOL, BSTR, Error as ComError, HRESULT, IUnknown, IUnknownImpl, PCWSTR, Result as ComResult,
    implement,
};

use crate::access::{self, AccessElement, Tree};
use crate::window::Window;
use crate::{Error, Result};

const UIA_ROOT_OBJECT_ID: i32 = -25;
const SUBCLASS_ID: usize = 0x676F73;
// `UiaAppendRuntimeId`: the provider's runtime id continues the host's.
const APPEND_RUNTIME_ID: i32 = 3;

fn not_available() -> ComError {
    HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32).into()
}

fn invalid() -> ComError {
    HRESULT(UIA_E_INVALIDOPERATION as i32).into()
}

fn hwnd_of(window: Window) -> Option<HWND> {
    match window.raw_handle() {
        Ok(crate::surface::RawHandle::Win32 { hwnd, .. }) => Some(HWND(hwnd)),
        _ => None,
    }
}

/// Answers `WM_GETOBJECT` for `window` from now on.
pub(crate) fn attach(window: Window) {
    let Some(hwnd) = hwnd_of(window) else { return };
    // OLE may already run on this thread (SDL starts it for drops); a
    // repeat start succeeds.
    // SAFETY: called on the window's own thread.
    unsafe {
        let _ = OleInitialize(None);
        let _ = SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, window.id().0 as usize);
    }
}

/// Disconnects every provider of `window`, which is about to close.
pub(crate) fn detach(window: Window) {
    let Some(hwnd) = hwnd_of(window) else { return };
    // SAFETY: the window is still alive on this thread.
    unsafe {
        UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None);
        let _ = RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID);
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if message == WM_GETOBJECT && lparam.0 as i32 == UIA_ROOT_OBJECT_ID {
        access::queried();
        let root: IRawElementProviderSimple = Element {
            window: data as u64,
            hwnd: hwnd.0 as isize,
            id: 0,
        }
        .into();
        // SAFETY: answering the message the system sent this window.
        return unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &root) };
    }
    if message == WM_NCDESTROY {
        // SAFETY: the window is being destroyed on this thread.
        unsafe {
            UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None);
            let _ = RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID);
        }
        access::forget(data as u64);
    }
    // SAFETY: forwards the message down the subclass chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// Whether any UI Automation client listens for events.
pub(crate) fn clients_listening() -> bool {
    // SAFETY: a plain query.
    unsafe { UiaClientsAreListening() }.as_bool()
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot,
    IInvokeProvider,
    IToggleProvider,
    IValueProvider,
    IRangeValueProvider,
    IExpandCollapseProvider,
    ISelectionItemProvider
)]
struct Element {
    window: u64,
    hwnd: isize,
    id: u64,
}

fn has(e: &AccessElement, action: &str) -> bool {
    e.actions.iter().any(|a| a == action)
}

fn is_text_role(role: &str) -> bool {
    matches!(
        role,
        "textfield" | "passwordfield" | "textarea" | "searchfield"
    )
}

fn toggles(e: &AccessElement) -> bool {
    matches!(
        e.role.as_str(),
        "checkbox" | "switch" | "togglebutton" | "menuitemcheckbox"
    ) && (e.checked >= 0 || e.role == "togglebutton")
}

fn selects(e: &AccessElement) -> bool {
    matches!(
        e.role.as_str(),
        "listitem" | "tab" | "treeitem" | "radio" | "menuitemradio" | "row"
    ) || has(e, "select")
}

fn supports(e: &AccessElement, pattern: UIA_PATTERN_ID) -> bool {
    match pattern {
        p if p == UIA_InvokePatternId => {
            !toggles(e) && (has(e, "press") || has(e, "activate") || has(e, "open"))
        }
        p if p == UIA_TogglePatternId => toggles(e),
        p if p == UIA_ValuePatternId => {
            !e.has_range && (is_text_role(&e.role) || e.role == "combobox" || !e.value.is_empty())
        }
        p if p == UIA_RangeValuePatternId => e.has_range,
        p if p == UIA_ExpandCollapsePatternId => e.expanded >= 0,
        p if p == UIA_SelectionItemPatternId => selects(e),
        _ => false,
    }
}

fn control_type(role: &str) -> i32 {
    match role {
        "button" | "togglebutton" | "colorwell" => 50000,
        "calendar" => 50001,
        "checkbox" | "switch" => 50002,
        "combobox" => 50003,
        "textfield" | "passwordfield" | "textarea" | "searchfield" => 50004,
        "link" => 50005,
        "image" | "canvas" => 50006,
        "listitem" => 50007,
        "listbox" => 50008,
        "menu" => 50009,
        "menubar" => 50010,
        "menuitem" | "menuitemcheckbox" | "menuitemradio" => 50011,
        "progressbar" => 50012,
        "radio" => 50013,
        "scrollbar" => 50014,
        "slider" => 50015,
        "spinbutton" => 50016,
        "statusbar" | "status" => 50017,
        "tablist" => 50018,
        "tab" => 50019,
        "label" | "paragraph" | "heading" | "cell" => 50020,
        "toolbar" => 50021,
        "tooltip" => 50022,
        "tree" => 50023,
        "treeitem" => 50024,
        "group" | "radiogroup" | "alert" => 50026,
        "splitter" => 50027,
        "grid" => 50028,
        "row" => 50029,
        "document" | "terminal" => 50030,
        "window" | "dialog" | "alertdialog" => 50032,
        "pane" | "tabpanel" | "scrollarea" => 50033,
        "columnheader" => 50035,
        "table" => 50036,
        "separator" => 50038,
        _ => 50025,
    }
}

fn toggle_state(e: &AccessElement) -> i32 {
    if e.checked == 1 || (e.checked < 0 && e.pressed) {
        1
    } else if e.checked == 2 {
        2
    } else {
        0
    }
}

fn expand_state(e: &AccessElement) -> i32 {
    match e.expanded {
        1 => 1,
        0 => 0,
        _ => 3,
    }
}

// What GetPropertyValue answers for `property` of `e`, the root when `root`.
fn property(e: &AccessElement, property: UIA_PROPERTY_ID, root: bool) -> VARIANT {
    let text = |s: &str| VARIANT::from(BSTR::from(s));
    match property.0 {
        // ControlType: the window's host says it is a window.
        30003 if !root => VARIANT::from(control_type(&e.role)),
        30005 if !e.name.is_empty() || !root => text(&e.name),
        30008 => VARIANT::from(e.focused),
        30009 => VARIANT::from(e.focusable),
        30010 => VARIANT::from(!e.disabled),
        30011 if !root => text(&e.id.to_string()),
        30013 if !e.description.is_empty() => text(&e.description),
        30019 => VARIANT::from(e.role == "passwordfield"),
        30024 => text("Gossamer"),
        30025 => VARIANT::from(e.required),
        30103 => VARIANT::from(!e.invalid),
        30135 if !e.live.is_empty() => VARIANT::from(match e.live.as_str() {
            "polite" => 1,
            "assertive" => 2,
            _ => 0,
        }),
        30152 if e.pos_in_set > 0 => VARIANT::from(e.pos_in_set as i32),
        30153 if e.set_size > 0 => VARIANT::from(e.set_size as i32),
        30154 if e.level > 0 => VARIANT::from(e.level as i32),
        // HeadingLevel: HeadingLevel1 is 80051.
        30173 if e.role == "heading" => VARIANT::from(80050 + e.level.clamp(1, 9) as i32),
        30174 => VARIANT::from(e.role == "dialog" || e.role == "alertdialog"),
        _ => VARIANT::default(),
    }
}

impl Element_Impl {
    fn tree(&self) -> Option<Tree> {
        access::tree(self.window)
    }

    fn data(&self) -> Option<AccessElement> {
        match self.tree() {
            Some(t) => t.get(if self.id == 0 { t.root } else { self.id }).cloned(),
            None if self.id == 0 => Some(AccessElement {
                id: 0,
                role: "window".to_string(),
                ..AccessElement::default()
            }),
            None => None,
        }
    }

    // Another element of this window; the root for the program's own root id.
    fn element(&self, id: u64) -> Element {
        let root = self.tree().map_or(0, |t| t.root);
        Element {
            window: self.window,
            hwnd: self.hwnd,
            id: if id == root { 0 } else { id },
        }
    }

    fn act(&self, action: &str, value: &str) -> ComResult<()> {
        let d = self.data().ok_or_else(not_available)?;
        if d.disabled {
            return Err(HRESULT(UIA_E_ELEMENTNOTENABLED as i32).into());
        }
        access::request(self.window, d.id, action, value);
        Ok(())
    }

    fn first_action(&self, wanted: &[&str]) -> ComResult<()> {
        let d = self.data().ok_or_else(not_available)?;
        match wanted.iter().find(|w| has(&d, w)) {
            Some(w) => self.act(w, ""),
            None => Err(invalid()),
        }
    }

    // The screen origin of the client area and the ratio of client pixels
    // to framebuffer pixels.
    fn client_frame(&self) -> Option<(f64, f64, f64)> {
        let hwnd = HWND(self.hwnd as _);
        let mut origin = POINT::default();
        let mut client = RECT::default();
        // SAFETY: the window is alive while its providers are asked.
        unsafe {
            let _ = ClientToScreen(hwnd, &mut origin);
            let _ = GetClientRect(hwnd, &mut client);
        }
        let (mut w, mut h) = (0, 0);
        // SAFETY: SDL looks the window up by id and fills the two sizes.
        unsafe {
            let handle = sdl3_sys::video::SDL_GetWindowFromID(sdl3_sys::video::SDL_WindowID(
                self.window as u32,
            ));
            if handle.is_null()
                || !sdl3_sys::video::SDL_GetWindowSizeInPixels(handle, &mut w, &mut h)
            {
                return None;
            }
        }
        let scale = if w > 0 && client.right > 0 {
            f64::from(client.right) / f64::from(w)
        } else {
            1.0
        };
        Some((f64::from(origin.x), f64::from(origin.y), scale))
    }

    fn fragment(&self, id: u64) -> ComResult<IRawElementProviderFragment> {
        if id == 0 {
            return Err(ComError::empty());
        }
        Ok(self.element(id).into())
    }
}

impl IRawElementProviderSimple_Impl for Element_Impl {
    fn ProviderOptions(&self) -> ComResult<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> ComResult<IUnknown> {
        let d = self.data().ok_or_else(not_available)?;
        if !supports(&d, pattern) {
            return Err(ComError::empty());
        }
        Ok(self.to_object().to_interface::<IUnknown>())
    }

    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> ComResult<VARIANT> {
        let d = self.data().ok_or_else(not_available)?;
        Ok(property(&d, id, self.id == 0))
    }

    fn HostRawElementProvider(&self) -> ComResult<IRawElementProviderSimple> {
        if self.id != 0 {
            return Err(ComError::empty());
        }
        // SAFETY: the window this root answers for.
        unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as _)) }
    }
}

impl IRawElementProviderFragment_Impl for Element_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> ComResult<IRawElementProviderFragment> {
        let Some(t) = self.tree() else {
            return Err(ComError::empty());
        };
        let me = if self.id == 0 { t.root } else { self.id };
        let d = t.get(me).ok_or_else(not_available)?;
        let target = if self.id != 0 && direction == NavigateDirection_Parent {
            t.parent(me)
        } else if self.id != 0 && direction == NavigateDirection_NextSibling {
            t.sibling(me, 1)
        } else if self.id != 0 && direction == NavigateDirection_PreviousSibling {
            t.sibling(me, -1)
        } else if direction == NavigateDirection_FirstChild {
            d.children.first().copied().unwrap_or(0)
        } else if direction == NavigateDirection_LastChild {
            d.children.last().copied().unwrap_or(0)
        } else {
            0
        };
        // The parent of a top-level element is the root, which is id 0 here.
        if target == 0 && direction == NavigateDirection_Parent && self.id != 0 {
            return Ok(self.element(t.root).into());
        }
        self.fragment(target)
    }

    fn GetRuntimeId(&self) -> ComResult<*mut windows::Win32::System::Com::SAFEARRAY> {
        // The root's runtime id is its window's, which the host provides.
        if self.id == 0 {
            return Ok(std::ptr::null_mut());
        }
        let parts = [
            APPEND_RUNTIME_ID,
            (self.id & 0x7FFF_FFFF) as i32,
            (self.id >> 31) as i32,
        ];
        // SAFETY: a three-element vector of 32-bit integers, filled in place.
        unsafe {
            let array = SafeArrayCreateVector(VT_I4, 0, 3);
            if array.is_null() {
                return Err(E_INVALIDARG.into());
            }
            for (i, part) in parts.iter().enumerate() {
                SafeArrayPutElement(array, &(i as i32), (part as *const i32).cast())?;
            }
            Ok(array)
        }
    }

    fn BoundingRectangle(&self) -> ComResult<UiaRect> {
        // The root's rectangle is its window's, which the host provides.
        if self.id == 0 {
            return Ok(UiaRect::default());
        }
        let d = self.data().ok_or_else(not_available)?;
        let Some((x, y, scale)) = self.client_frame() else {
            return Ok(UiaRect::default());
        };
        Ok(UiaRect {
            left: x + d.x as f64 * scale,
            top: y + d.y as f64 * scale,
            width: d.width as f64 * scale,
            height: d.height as f64 * scale,
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> ComResult<*mut windows::Win32::System::Com::SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> ComResult<()> {
        self.act("focus", "")
    }

    fn FragmentRoot(&self) -> ComResult<IRawElementProviderFragmentRoot> {
        Ok(Element {
            window: self.window,
            hwnd: self.hwnd,
            id: 0,
        }
        .into())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Element_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> ComResult<IRawElementProviderFragment> {
        let (Some(t), Some((left, top, scale))) = (self.tree(), self.client_frame()) else {
            return Err(ComError::empty());
        };
        self.fragment(t.hit(((x - left) / scale) as i64, ((y - top) / scale) as i64))
    }

    fn GetFocus(&self) -> ComResult<IRawElementProviderFragment> {
        let Some(t) = self.tree() else {
            return Err(ComError::empty());
        };
        if t.focus == 0 || t.focus == t.root || t.get(t.focus).is_none() {
            return Err(ComError::empty());
        }
        self.fragment(t.focus)
    }
}

impl IInvokeProvider_Impl for Element_Impl {
    fn Invoke(&self) -> ComResult<()> {
        self.first_action(&["press", "activate", "open"])
    }
}

impl IToggleProvider_Impl for Element_Impl {
    fn Toggle(&self) -> ComResult<()> {
        self.first_action(&["toggle", "press"])
    }

    fn ToggleState(&self) -> ComResult<ToggleState> {
        Ok(ToggleState(toggle_state(
            &self.data().ok_or_else(not_available)?,
        )))
    }
}

impl IValueProvider_Impl for Element_Impl {
    fn SetValue(&self, text: &PCWSTR) -> ComResult<()> {
        let d = self.data().ok_or_else(not_available)?;
        if d.read_only {
            return Err(invalid());
        }
        // SAFETY: UI Automation passes a NUL-terminated wide string or null.
        let value = if text.is_null() {
            String::new()
        } else {
            unsafe { text.to_string() }.unwrap_or_default()
        };
        self.act("set-text", &value)
    }

    fn Value(&self) -> ComResult<BSTR> {
        let d = self.data().ok_or_else(not_available)?;
        let value = if d.role == "passwordfield" {
            String::new()
        } else {
            d.text_value()
        };
        Ok(BSTR::from(value.as_str()))
    }

    fn IsReadOnly(&self) -> ComResult<BOOL> {
        let d = self.data().ok_or_else(not_available)?;
        Ok((d.read_only || !is_text_role(&d.role) && d.role != "combobox").into())
    }
}

impl IRangeValueProvider_Impl for Element_Impl {
    fn SetValue(&self, value: f64) -> ComResult<()> {
        let d = self.data().ok_or_else(not_available)?;
        if d.read_only || value < d.range_min || value > d.range_max {
            return Err(E_INVALIDARG.into());
        }
        self.act("set-value", &value.to_string())
    }

    fn Value(&self) -> ComResult<f64> {
        Ok(self.data().ok_or_else(not_available)?.range_now)
    }

    fn IsReadOnly(&self) -> ComResult<BOOL> {
        let d = self.data().ok_or_else(not_available)?;
        Ok((d.read_only || d.role == "progressbar").into())
    }

    fn Maximum(&self) -> ComResult<f64> {
        Ok(self.data().ok_or_else(not_available)?.range_max)
    }

    fn Minimum(&self) -> ComResult<f64> {
        Ok(self.data().ok_or_else(not_available)?.range_min)
    }

    // The program does not report its steps, so a tenth and a hundredth of
    // the range stand in for them.
    fn LargeChange(&self) -> ComResult<f64> {
        let d = self.data().ok_or_else(not_available)?;
        Ok((d.range_max - d.range_min) / 10.0)
    }

    fn SmallChange(&self) -> ComResult<f64> {
        let d = self.data().ok_or_else(not_available)?;
        Ok((d.range_max - d.range_min) / 100.0)
    }
}

impl Element_Impl {
    fn expand_to(&self, wanted: i64) -> ComResult<()> {
        let d = self.data().ok_or_else(not_available)?;
        if d.expanded == wanted {
            return Ok(());
        }
        let named = if wanted == 1 { "expand" } else { "collapse" };
        if has(&d, named) {
            return self.act(named, "");
        }
        self.first_action(&["toggle-expanded", "press"])
    }
}

impl IExpandCollapseProvider_Impl for Element_Impl {
    fn Expand(&self) -> ComResult<()> {
        self.expand_to(1)
    }

    fn Collapse(&self) -> ComResult<()> {
        self.expand_to(0)
    }

    fn ExpandCollapseState(&self) -> ComResult<ExpandCollapseState> {
        Ok(ExpandCollapseState(expand_state(
            &self.data().ok_or_else(not_available)?,
        )))
    }
}

impl ISelectionItemProvider_Impl for Element_Impl {
    fn Select(&self) -> ComResult<()> {
        self.first_action(&["select", "press"])
    }

    fn AddToSelection(&self) -> ComResult<()> {
        self.first_action(&["select", "press"])
    }

    fn RemoveFromSelection(&self) -> ComResult<()> {
        Err(invalid())
    }

    fn IsSelected(&self) -> ComResult<BOOL> {
        Ok(self.data().ok_or_else(not_available)?.selected.into())
    }

    fn SelectionContainer(&self) -> ComResult<IRawElementProviderSimple> {
        let Some(t) = self.tree() else {
            return Err(ComError::empty());
        };
        let mut at = t.parent(self.id);
        while let Some(p) = t.get(at) {
            if matches!(
                p.role.as_str(),
                "listbox" | "tablist" | "tree" | "radiogroup" | "grid" | "table" | "menu"
            ) {
                return Ok(self.element(at).into());
            }
            at = t.parent(at);
        }
        Err(ComError::empty())
    }
}

// Runs `raise` on a provider for element `id` of `window`.
fn with_element(window: Window, id: u64, raise: impl FnOnce(&IRawElementProviderSimple)) {
    let Some(hwnd) = hwnd_of(window) else { return };
    let key = u64::from(window.id().0);
    let root = access::tree(key).map_or(0, |t| t.root);
    let p: IRawElementProviderSimple = Element {
        window: key,
        hwnd: hwnd.0 as isize,
        id: if id == root { 0 } else { id },
    }
    .into();
    raise(&p);
}

fn property_changed(window: Window, id: u64, property: i32, old: VARIANT, new: VARIANT) {
    // SAFETY: raising an event on a live provider.
    with_element(window, id, |p| unsafe {
        let _ = UiaRaiseAutomationPropertyChangedEvent(p, UIA_PROPERTY_ID(property), &old, &new);
    });
}

fn raise(window: Window, id: u64, event: i32) {
    // SAFETY: raising an event on a live provider.
    with_element(window, id, |p| unsafe {
        let _ = UiaRaiseAutomationEvent(p, UIA_EVENT_ID(event));
    });
}

fn children_invalidated(window: Window, root: u64) {
    let mut runtime = [APPEND_RUNTIME_ID, 0];
    // SAFETY: StructureChangeType_ChildrenInvalidated on the root.
    with_element(window, root, |p| unsafe {
        let _ = UiaRaiseStructureChangedEvent(
            p,
            StructureChangeType_ChildrenInvalidated,
            runtime.as_mut_ptr(),
            2,
        );
    });
}

/// Raises the events a screen reader needs to follow the change from
/// `previous` to `next`.
pub(crate) fn tree_changed(window: Window, previous: Option<Tree>, next: &Tree) -> Result<()> {
    if !clients_listening() {
        return Ok(());
    }
    let Some(before) = previous else {
        children_invalidated(window, next.root);
        return Ok(());
    };
    let text = |s: &str| VARIANT::from(BSTR::from(s));
    let mut structure = before.elements.len() != next.elements.len();
    for (&id, e) in &next.elements {
        let Some(old) = before.get(id) else {
            structure = true;
            continue;
        };
        structure |= old.children != e.children;
        if old.name != e.name {
            property_changed(window, id, 30005, text(&old.name), text(&e.name));
        }
        if old.value != e.value && e.role != "passwordfield" {
            property_changed(window, id, 30045, text(&old.value), text(&e.value));
        }
        if old.disabled != e.disabled {
            property_changed(
                window,
                id,
                30010,
                VARIANT::from(!old.disabled),
                VARIANT::from(!e.disabled),
            );
        }
        if toggle_state(old) != toggle_state(e) && toggles(e) {
            property_changed(
                window,
                id,
                30086,
                VARIANT::from(toggle_state(old)),
                VARIANT::from(toggle_state(e)),
            );
        }
        if old.expanded != e.expanded && e.expanded >= 0 {
            property_changed(
                window,
                id,
                30070,
                VARIANT::from(expand_state(old)),
                VARIANT::from(expand_state(e)),
            );
        }
        if e.has_range && old.range_now != e.range_now {
            property_changed(
                window,
                id,
                30047,
                VARIANT::from(old.range_now),
                VARIANT::from(e.range_now),
            );
        }
        if e.selected && !old.selected {
            // SelectionItem_ElementSelectedEvent.
            raise(window, id, 20012);
        }
        if !e.live.is_empty() && (old.name != e.name || old.value != e.value) {
            // LiveRegionChangedEvent.
            raise(window, id, 20024);
        }
    }
    if structure {
        children_invalidated(window, next.root);
    }
    if next.focus != 0 && next.focus != before.focus {
        focus_changed(window, next.focus)?;
    }
    Ok(())
}

/// Tells screen readers element `id` of `window` took the focus.
pub(crate) fn focus_changed(window: Window, id: u64) -> Result<()> {
    if clients_listening() {
        // UIA_AutomationFocusChangedEventId.
        raise(window, id, 20005);
    }
    Ok(())
}

/// Asks screen readers to speak `text`, interrupting when `assertive`.
pub(crate) fn announce(window: Window, text: &str, assertive: bool) -> Result<()> {
    let root = access::tree(u64::from(window.id().0)).map_or(0, |t| t.root);
    let mut result = Ok(());
    let processing = if assertive {
        NotificationProcessing_ImportantMostRecent
    } else {
        NotificationProcessing_MostRecent
    };
    // SAFETY: raising an event on a live provider.
    with_element(window, root, |p| {
        result = unsafe {
            UiaRaiseNotificationEvent(
                p,
                NotificationKind_Other,
                processing,
                &BSTR::from(text),
                &BSTR::from("windowing.announcement"),
            )
        };
    });
    result.map_err(|e| {
        Error::new(format!(
            "announcing: {e} (notifications need Windows 10 1709 or later)"
        ))
    })
}
