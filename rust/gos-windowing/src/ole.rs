//! OLE drag sources on Windows (`DoDragDrop`).

#[cfg(target_os = "windows")]
mod com;
#[cfg(target_os = "windows")]
pub(crate) use com::start_drag;

#[cfg(not(target_os = "windows"))]
pub(crate) fn start_drag(
    _items: Vec<crate::drag::DragItem>,
    _operation: crate::drag::DragOperation,
    _finished: u32,
) -> crate::Result<crate::drag::DragId> {
    Err(crate::Error::new("OLE drags need Windows"))
}
