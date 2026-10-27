// Dragging data out of a window on Windows: OLE's `DoDragDrop` over a data
// object and a drop source this module implements. OLE runs the drag in a
// modal loop on the main thread until the button is released or Escape is
// pressed, asking the drop source whether to go on and the target asking
// the data object for the formats it reads.
//
// Each drag item is offered under a registered format named by its MIME
// type, and as the standard format Windows programs read: text as
// `CF_UNICODETEXT`, `text/uri-list` as `CF_HDROP` for files and
// `UniformResourceLocatorW` for the first URI, and `text/html` as "HTML
// Format".

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows::Win32::Foundation::{
    DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_FORMATETC, E_NOTIMPL, HGLOBAL, OLE_E_ADVISENOTSUPPORTED,
};
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl,
    IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize,
};
use windows::Win32::System::SystemServices::{
    MK_LBUTTON, MK_MBUTTON, MK_RBUTTON, MODIFIERKEYS_FLAGS,
};
use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
use windows::core::{BOOL, HRESULT, PCWSTR, Ref, implement};

use crate::drag::{DragId, DragItem, DragOperation, operation_code};
use crate::instance::path_unescape;
use crate::{Error, Result};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One format the data object serves and its bytes.
#[derive(Clone)]
struct Rendering {
    format: u16,
    bytes: Vec<u8>,
}

#[implement(IDataObject)]
struct DataObject {
    renderings: Vec<Rendering>,
}

#[implement(IDropSource)]
struct DropSource;

fn asked_format(asked: *const FORMATETC) -> Option<FORMATETC> {
    // SAFETY: OLE passes a valid FORMATETC or null.
    let f = unsafe { asked.as_ref() }?;
    (f.dwAspect == DVASPECT_CONTENT.0 && f.tymed & TYMED_HGLOBAL.0 as u32 != 0).then_some(*f)
}

impl DataObject_Impl {
    fn rendering(&self, asked: *const FORMATETC) -> Option<&Rendering> {
        let f = asked_format(asked)?;
        self.renderings.iter().find(|r| r.format == f.cfFormat)
    }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, asked: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let r = self.rendering(asked).ok_or(DV_E_FORMATETC)?;
        // SAFETY: a movable block as large as the rendering, filled under
        // its lock; the receiver owns and frees it.
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, r.bytes.len().max(1))?;
            let at = GlobalLock(memory) as *mut u8;
            std::ptr::copy_nonoverlapping(r.bytes.as_ptr(), at, r.bytes.len());
            let _ = GlobalUnlock(memory);
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 {
                    hGlobal: HGLOBAL(memory.0),
                },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            })
        }
    }

    fn GetDataHere(
        &self,
        _asked: *const FORMATETC,
        _medium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, asked: *const FORMATETC) -> HRESULT {
        if self.rendering(asked).is_some() {
            HRESULT(0)
        } else {
            DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(&self, _asked: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        // SAFETY: OLE passes a writable FORMATETC.
        if let Some(out) = unsafe { out.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        _asked: *const FORMATETC,
        _medium: *const STGMEDIUM,
        _release: BOOL,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let formats: Vec<FORMATETC> = self
            .renderings
            .iter()
            .map(|r| FORMATETC {
                cfFormat: r.format,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            })
            .collect();
        // SAFETY: the shell copies the formats into the enumerator it answers.
        unsafe { SHCreateStdEnumFmtEtc(&formats) }
    }

    fn DAdvise(
        &self,
        _asked: *const FORMATETC,
        _flags: u32,
        _sink: Ref<IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _connection: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if keys.0 & (MK_LBUTTON.0 | MK_RBUTTON.0 | MK_MBUTTON.0) == 0 {
            return DRAGDROP_S_DROP;
        }
        HRESULT(0)
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn wide_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The Windows path a `file://` URI names, or `None` for any other URI.
pub(crate) fn windows_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let unescaped = rest
        .split('/')
        .map(path_unescape)
        .collect::<Vec<_>>()
        .join("\\");
    if let Some(drive) = unescaped.strip_prefix('\\') {
        return (drive.as_bytes().get(1) == Some(&b':')).then(|| drive.to_string());
    }
    if let Some(local) = rest.strip_prefix("localhost/") {
        return windows_path(&format!("file:///{local}"));
    }
    Some(format!("\\\\{unescaped}"))
}

// `DROPFILES` with wide names, followed by each path, NUL-terminated, and a
// closing NUL.
fn hdrop(paths: &[String]) -> Vec<u8> {
    let mut out: Vec<u8> = [20u32, 0, 0, 0, 1]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    for p in paths {
        out.extend(wide_bytes(p));
    }
    out.extend([0, 0]);
    out
}

/// `fragment` in the clipboard's "HTML Format": a header of byte offsets,
/// then the fragment inside a document.
pub(crate) fn html_format(fragment: &str) -> Vec<u8> {
    let before = "<html><body>\r\n<!--StartFragment-->";
    let after = "<!--EndFragment-->\r\n</body>\r\n</html>";
    let header = |a: usize, b: usize, c: usize, d: usize| {
        format!(
            "Version:0.9\r\nStartHTML:{a:010}\r\nEndHTML:{b:010}\r\nStartFragment:{c:010}\r\nEndFragment:{d:010}\r\n"
        )
    };
    let header_len = header(0, 0, 0, 0).len();
    let start_fragment = header_len + before.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + after.len();
    let mut out = format!(
        "{}{before}{fragment}{after}",
        header(header_len, end_html, start_fragment, end_fragment)
    )
    .into_bytes();
    out.push(0);
    out
}

fn registered(name: &str) -> u16 {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated wide name.
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) as u16 }
}

fn add(out: &mut Vec<Rendering>, format: u16, bytes: Vec<u8>) {
    if format != 0 && !out.iter().any(|r| r.format == format) {
        out.push(Rendering { format, bytes });
    }
}

fn is_text(mime: &str) -> bool {
    matches!(
        mime,
        "text/plain;charset=utf-8" | "text/plain" | "UTF8_STRING" | "TEXT" | "STRING"
    )
}

fn renderings(items: &[DragItem]) -> Vec<Rendering> {
    let mut out = vec![];
    // Each item's own bytes under a format named by its MIME type come
    // first: targets that speak MIME types (SDL's among them) read those,
    // and the standard formats below serve the rest.
    for item in items {
        add(&mut out, registered(&item.mime), item.data.clone());
    }
    for item in items.iter().filter(|i| is_text(&i.mime)) {
        add(
            &mut out,
            CF_UNICODETEXT.0,
            wide_bytes(&String::from_utf8_lossy(&item.data)),
        );
    }
    for item in items {
        let text = String::from_utf8_lossy(&item.data);
        if item.mime == "text/uri-list" {
            let uris: Vec<&str> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();
            let paths: Vec<String> = uris.iter().filter_map(|u| windows_path(u)).collect();
            if !paths.is_empty() && paths.len() == uris.len() {
                add(&mut out, CF_HDROP.0, hdrop(&paths));
            }
            if let Some(first) = uris.first() {
                add(
                    &mut out,
                    registered("UniformResourceLocatorW"),
                    wide_bytes(first),
                );
                add(&mut out, CF_UNICODETEXT.0, wide_bytes(&uris.join("\r\n")));
            }
        } else if item.mime == "text/html" {
            add(&mut out, registered("HTML Format"), html_format(&text));
        }
    }
    out
}

fn performed(effect: DROPEFFECT) -> Option<DragOperation> {
    if effect.0 & DROPEFFECT_MOVE.0 != 0 {
        Some(DragOperation::Move)
    } else if effect.0 & DROPEFFECT_COPY.0 != 0 {
        Some(DragOperation::Copy)
    } else if effect.0 & DROPEFFECT_LINK.0 != 0 {
        Some(DragOperation::Link)
    } else {
        None
    }
}

/// Runs a drag of `items` to its end, then queues `Event::DragFinished` as
/// the event type `finished`.
pub(crate) fn start_drag(
    items: Vec<DragItem>,
    operation: DragOperation,
    finished: u32,
) -> Result<DragId> {
    if ACTIVE.swap(true, Ordering::Relaxed) {
        return Err(Error::new("a drag is already in progress"));
    }
    let outcome = run_drag(&items, operation);
    ACTIVE.store(false, Ordering::Relaxed);
    let (id, done) = outcome?;
    crate::waker::push_user_event(finished, 0, operation_code(done), id, 0)?;
    Ok(DragId(id))
}

fn run_drag(items: &[DragItem], operation: DragOperation) -> Result<(u64, Option<DragOperation>)> {
    // SAFETY: OLE starts once per thread; a repeat succeeds too.
    unsafe { OleInitialize(None) }
        .map_err(|e| Error::new(format!("starting OLE for the drag: {e}")))?;
    let renderings = renderings(items);
    if renderings.is_empty() {
        return Err(Error::new(
            "none of the drag's items has a Windows representation",
        ));
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let data: IDataObject = DataObject { renderings }.into();
    let source: IDropSource = DropSource.into();
    let allowed = match operation {
        DragOperation::Copy => DROPEFFECT_COPY,
        DragOperation::Move => DROPEFFECT_MOVE,
        DragOperation::Link => DROPEFFECT_LINK,
    };
    let mut effect = DROPEFFECT(0);
    // SAFETY: both objects live across the modal drag loop.
    let result = unsafe { DoDragDrop(&data, &source, allowed, &mut effect) };
    Ok((
        id,
        if result == DRAGDROP_S_DROP {
            performed(effect)
        } else {
            None
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_become_windows_paths() {
        assert_eq!(
            windows_path("file:///C:/a%20b/c.txt").as_deref(),
            Some("C:\\a b\\c.txt")
        );
        assert_eq!(
            windows_path("file://server/share/x").as_deref(),
            Some("\\\\server\\share\\x")
        );
        assert_eq!(windows_path("https://x"), None);
    }
}
