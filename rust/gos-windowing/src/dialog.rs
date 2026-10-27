//! The system's file and folder dialogs, and message boxes. A file dialog
//! does not block: it answers a `DialogId` at once, and the choice arrives
//! later as `Event::DialogClosed` while the loop keeps running.

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use sdl3_sys::dialog::{
    SDL_DialogFileFilter, SDL_FILEDIALOG_OPENFILE, SDL_FILEDIALOG_OPENFOLDER,
    SDL_FILEDIALOG_SAVEFILE, SDL_FileDialogType, SDL_PROP_FILE_DIALOG_FILTERS_POINTER,
    SDL_PROP_FILE_DIALOG_LOCATION_STRING, SDL_PROP_FILE_DIALOG_MANY_BOOLEAN,
    SDL_PROP_FILE_DIALOG_NFILTERS_NUMBER, SDL_PROP_FILE_DIALOG_TITLE_STRING,
    SDL_PROP_FILE_DIALOG_WINDOW_POINTER, SDL_ShowFileDialogWithProperties,
};
use sdl3_sys::error::SDL_GetError;
use sdl3_sys::messagebox::{
    SDL_MESSAGEBOX_BUTTON_ESCAPEKEY_DEFAULT, SDL_MESSAGEBOX_BUTTON_RETURNKEY_DEFAULT,
    SDL_MESSAGEBOX_ERROR, SDL_MESSAGEBOX_INFORMATION, SDL_MESSAGEBOX_WARNING,
    SDL_MessageBoxButtonData, SDL_MessageBoxButtonFlags, SDL_MessageBoxData, SDL_MessageBoxFlags,
    SDL_ShowMessageBox, SDL_ShowSimpleMessageBox,
};

use crate::window::{Properties, Window};
use crate::{App, Error, Result, event, support, waker};

/// A dialog the application opened, as `Event::DialogClosed` names it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct DialogId(pub u64);

impl From<DialogId> for u64 {
    fn from(id: DialogId) -> u64 {
        id.0
    }
}

impl From<u64> for DialogId {
    fn from(n: u64) -> DialogId {
        DialogId(n)
    }
}

/// A file-type filter: a label, and the extensions it admits separated by
/// `;` (`"png;jpg"`), or `*` for every file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Filter {
    pub name: String,
    pub pattern: String,
}

/// How a dialog ended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DialogChoice {
    /// The user chose `paths`; `filter` is the index of the filter in use,
    /// where the platform reports it.
    Chosen {
        paths: Vec<String>,
        filter: Option<i64>,
    },
    Cancelled,
    /// The dialog could not be shown or failed.
    Failed {
        reason: String,
    },
}

/// The kind of message a message box shows, which picks its icon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MessageKind {
    #[default]
    Info,
    Warning,
    Error,
}

/// What a file or folder dialog shows. `FileDialog::default()` opens with
/// the platform's defaults.
#[derive(Debug, Clone, Default)]
pub struct FileDialog {
    pub title: String,
    /// The folder to start in, or for a save dialog a file to propose.
    pub location: String,
    pub filters: Vec<Filter>,
    /// Whether several files or folders may be chosen.
    pub many: bool,
    /// The window the dialog belongs to, which it stays above.
    pub parent: Option<Window>,
}

// What a pending dialog's completion needs: where to deliver the choice, and
// the C strings of the filter list, which have to outlive the dialog.
struct Request {
    epoch: u64,
    cancelled: bool,
    event_type: u32,
    _names: Vec<CString>,
    _filters: Vec<SDL_DialogFileFilter>,
}

// SAFETY: the filter list's pointers refer only into `_names`, which the
// request owns; it is touched by one thread at a time under PENDING's lock.
unsafe impl Send for Request {}

// Serialize completion publication with session shutdown. Native callbacks may
// outlive SDL sessions; their borrowed filter buffers must survive until callback.
static DELIVERY: Mutex<()> = Mutex::new(());
static EPOCH: AtomicU64 = AtomicU64::new(1);
const MAX_PENDING: usize = 64;

pub(crate) fn shutdown() {
    let _delivery = DELIVERY.lock();
    EPOCH.fetch_add(1, Ordering::AcqRel);
    if let Some(pending) = PENDING.lock().as_mut() {
        for request in pending.values_mut() {
            request.cancelled = true;
        }
    }
    if let Some(choices) = CHOICES.lock().as_mut() {
        choices.clear();
        choices.shrink_to_fit();
    }
}

static NEXT_DIALOG: AtomicU64 = AtomicU64::new(1);
static PENDING: Mutex<Option<HashMap<u64, Request>>> = Mutex::new(None);
// Choices queued for the loop, keyed by the token the queued event carries.
static CHOICES: Mutex<Option<HashMap<u64, DialogChoice>>> = Mutex::new(None);
static NEXT_CHOICE: AtomicU64 = AtomicU64::new(1);

// SDL calls this once per dialog, on whatever thread the platform's dialog
// finishes on, possibly before `SDL_ShowFileDialogWithProperties` returns.
unsafe extern "C" fn dialog_done(
    userdata: *mut c_void,
    files: *const *const c_char,
    filter: c_int,
) {
    let id = userdata as u64;
    let Some(request) = PENDING.lock().as_mut().and_then(|p| p.remove(&id)) else {
        return;
    };
    if request.cancelled {
        return;
    }
    let choice = if files.is_null() {
        DialogChoice::Failed {
            reason: support::read_text(SDL_GetError()),
        }
    } else {
        let mut paths = Vec::new();
        let mut i = 0;
        loop {
            // SAFETY: SDL answers a NULL-terminated array of C strings.
            let entry = unsafe { *files.add(i) };
            if entry.is_null() {
                break;
            }
            paths.push(support::read_text(entry));
            i += 1;
        }
        if paths.is_empty() {
            DialogChoice::Cancelled
        } else {
            DialogChoice::Chosen {
                paths,
                filter: (filter >= 0).then_some(i64::from(filter)),
            }
        }
    };
    // SDL's callback has no way to answer an error; a choice the queue
    // refused is dropped by `queue_choice`.
    let _delivery = DELIVERY.lock();
    if request.epoch == EPOCH.load(Ordering::Acquire) {
        let _ = queue_choice(request.event_type, DialogId(id), choice);
    }
}

/// Queues `choice` as the `DialogClosed` of `dialog` on `event_type`.
pub(crate) fn queue_choice(event_type: u32, dialog: DialogId, choice: DialogChoice) -> Result<()> {
    let token = NEXT_CHOICE.fetch_add(1, Ordering::Relaxed);
    {
        let mut choices = CHOICES.lock();
        let choices = choices.get_or_insert_with(HashMap::new);
        if choices.len() >= MAX_PENDING {
            return Err(Error::new("dialog result queue is full"));
        }
        choices.insert(token, choice);
    }
    let pushed = waker::push_user_event(event_type, 0, 0, token, dialog.0);
    if pushed.is_err() {
        let _ = take_choice(token);
    }
    pushed
}

/// The choice `queue_choice` stored under `token`, removed.
pub(crate) fn take_choice(token: u64) -> DialogChoice {
    CHOICES
        .lock()
        .as_mut()
        .and_then(|c| c.remove(&token))
        .unwrap_or(DialogChoice::Cancelled)
}

impl App {
    /// Cancels delivery and queues Cancelled once. SDL has no portable API to
    /// dismiss the native chooser; its borrowed buffers live until it completes.
    pub fn cancel_dialog(&self, id: DialogId) -> Result<()> {
        support::require_main_thread("App::cancel_dialog");
        let _delivery = DELIVERY.lock();
        let event_type = {
            let mut pending = PENDING.lock();
            let request = pending
                .as_mut()
                .and_then(|requests| requests.get_mut(&id.0))
                .ok_or_else(|| Error::new("dialog is no longer pending"))?;
            if request.cancelled || request.epoch != EPOCH.load(Ordering::Acquire) {
                return Err(Error::new("dialog is already cancelled"));
            }
            request.cancelled = true;
            request.event_type
        };
        queue_choice(event_type, id, DialogChoice::Cancelled)
    }
    /// Opens the system's file-open dialog; the choice arrives as
    /// `Event::DialogClosed` with the answered id.
    pub fn open_file_dialog(&self, options: &FileDialog) -> Result<DialogId> {
        show(SDL_FILEDIALOG_OPENFILE, self.event_base, options)
    }

    /// Opens the system's save dialog.
    pub fn save_file_dialog(&self, options: &FileDialog) -> Result<DialogId> {
        show(SDL_FILEDIALOG_SAVEFILE, self.event_base, options)
    }

    /// Opens the system's folder chooser; `filters` do not apply.
    pub fn open_folder_dialog(&self, options: &FileDialog) -> Result<DialogId> {
        show(SDL_FILEDIALOG_OPENFOLDER, self.event_base, options)
    }
}

fn show(kind: SDL_FileDialogType, event_base: u32, options: &FileDialog) -> Result<DialogId> {
    support::require_main_thread("App::*_dialog");
    if PENDING.lock().as_ref().map_or(0, HashMap::len) >= MAX_PENDING {
        return Err(Error::new("too many pending native dialogs"));
    }
    if options.filters.len() > 256
        || options.title.len() > 64 * 1024
        || options.location.len() > 64 * 1024
        || options
            .filters
            .iter()
            .any(|filter| filter.name.len() > 4096 || filter.pattern.len() > 4096)
    {
        return Err(Error::new("dialog options exceed their byte budget"));
    }
    let props = Properties::new("creating dialog properties")?;
    if !options.title.is_empty() {
        props.set_string(SDL_PROP_FILE_DIALOG_TITLE_STRING, &options.title)?;
    }
    if !options.location.is_empty() {
        props.set_string(SDL_PROP_FILE_DIALOG_LOCATION_STRING, &options.location)?;
    }
    props.set_bool(SDL_PROP_FILE_DIALOG_MANY_BOOLEAN, options.many)?;
    if let Some(parent) = options.parent {
        props.set_pointer(
            SDL_PROP_FILE_DIALOG_WINDOW_POINTER,
            parent.handle.cast::<c_void>(),
        )?;
    }
    let mut names = Vec::with_capacity(options.filters.len() * 2);
    for f in &options.filters {
        names.push(support::c_text(&f.name, "a filter name")?);
        names.push(support::c_text(&f.pattern, "a filter pattern")?);
    }
    let filters: Vec<SDL_DialogFileFilter> = names
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[name, pattern]| SDL_DialogFileFilter {
            name: name.as_ptr(),
            pattern: pattern.as_ptr(),
        })
        .collect();
    if !filters.is_empty() {
        props.set_pointer(
            SDL_PROP_FILE_DIALOG_FILTERS_POINTER,
            filters.as_ptr().cast_mut().cast::<c_void>(),
        )?;
        props.set_number(SDL_PROP_FILE_DIALOG_NFILTERS_NUMBER, filters.len() as i64)?;
    }
    let id = NEXT_DIALOG.fetch_add(1, Ordering::Relaxed);
    // The CStrings and the filter array live in heap buffers that moving the
    // request leaves in place, so the pointers set above stay valid.
    PENDING.lock().get_or_insert_with(HashMap::new).insert(
        id,
        Request {
            epoch: EPOCH.load(Ordering::Acquire),
            cancelled: false,
            event_type: event_base + event::DIALOG_OFFSET,
            _names: names,
            _filters: filters,
        },
    );
    // SAFETY: the callback and its context stay valid until the callback runs.
    unsafe {
        SDL_ShowFileDialogWithProperties(
            kind,
            Some(dialog_done),
            id as usize as *mut c_void,
            props.0,
        )
    };
    Ok(DialogId(id))
}

fn message_flags(kind: MessageKind) -> SDL_MessageBoxFlags {
    match kind {
        MessageKind::Info => SDL_MESSAGEBOX_INFORMATION,
        MessageKind::Warning => SDL_MESSAGEBOX_WARNING,
        MessageKind::Error => SDL_MESSAGEBOX_ERROR,
    }
}

/// Shows a message with an OK button and waits until it is dismissed. Works
/// before `init` too, to report a failure to start.
pub fn message_box(title: &str, message: &str, kind: MessageKind) -> Result<()> {
    support::require_main_thread("windowing::dialog::message_box");
    let title_text = support::c_text(title, "a message box title")?;
    let message_text = support::c_text(message, "a message")?;
    support::check(
        // SAFETY: both strings outlive the call; no parent window.
        unsafe {
            SDL_ShowSimpleMessageBox(
                message_flags(kind),
                title_text.as_ptr(),
                message_text.as_ptr(),
                std::ptr::null_mut(),
            )
        },
        "showing a message box",
    )
}

/// A message with buttons to choose from, for `ask`.
#[derive(Debug, Clone, Default)]
pub struct Question {
    pub title: String,
    pub message: String,
    pub buttons: Vec<String>,
    pub kind: MessageKind,
    pub parent: Option<Window>,
    /// The button Return picks.
    pub default_button: Option<i64>,
    /// The button Escape picks.
    pub cancel_button: Option<i64>,
}

/// Shows `question` and waits for a button: answers its index, or `None`
/// when the box was closed without one.
pub fn ask(question: &Question) -> Result<Option<i64>> {
    support::require_main_thread("windowing::dialog::ask");
    if question.buttons.is_empty() {
        return Err(Error::new("a question needs at least one button"));
    }
    let labels = question
        .buttons
        .iter()
        .map(|label| support::c_text(label, "a button label"))
        .collect::<Result<Vec<_>>>()?;
    let buttons: Vec<SDL_MessageBoxButtonData> = labels
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let mut flags = 0;
            if question.default_button == Some(i as i64) {
                flags |= SDL_MESSAGEBOX_BUTTON_RETURNKEY_DEFAULT.0;
            }
            if question.cancel_button == Some(i as i64) {
                flags |= SDL_MESSAGEBOX_BUTTON_ESCAPEKEY_DEFAULT.0;
            }
            SDL_MessageBoxButtonData {
                flags: SDL_MessageBoxButtonFlags(flags),
                buttonID: i as c_int,
                text: text.as_ptr(),
            }
        })
        .collect();
    let title = support::c_text(&question.title, "a message box title")?;
    let message = support::c_text(&question.message, "a message")?;
    let data = SDL_MessageBoxData {
        flags: message_flags(question.kind),
        window: question.parent.map_or(std::ptr::null_mut(), |w| w.handle),
        title: title.as_ptr(),
        message: message.as_ptr(),
        numbuttons: buttons.len() as c_int,
        buttons: buttons.as_ptr(),
        colorScheme: std::ptr::null(),
    };
    let mut chosen: c_int = -1;
    // SAFETY: every pointer in data outlives the call.
    let shown = unsafe { SDL_ShowMessageBox(&data, &mut chosen) };
    support::check(shown, "showing a message box")?;
    Ok((chosen >= 0).then_some(i64::from(chosen)))
}

#[cfg(test)]
mod tests {
    use super::{DialogChoice, take_choice};

    fn round_trip(choice: DialogChoice) -> DialogChoice {
        let token = super::NEXT_CHOICE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        super::CHOICES
            .lock()
            .get_or_insert_with(Default::default)
            .insert(token, choice);
        take_choice(token)
    }

    #[test]
    fn late_native_callbacks_release_their_request_without_publishing_to_a_new_session() {
        let id = super::NEXT_DIALOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        super::PENDING
            .lock()
            .get_or_insert_with(Default::default)
            .insert(
                id,
                super::Request {
                    epoch: 0,
                    cancelled: false,
                    event_type: 0,
                    _names: Vec::new(),
                    _filters: Vec::new(),
                },
            );
        // SAFETY: null files is SDL's documented failure callback shape. Epoch 0
        // is stale, so this must never publish into SDL's live event queue.
        unsafe {
            super::dialog_done(id as *mut std::ffi::c_void, std::ptr::null(), -1);
        }
        assert!(!super::PENDING.lock().as_ref().unwrap().contains_key(&id));
        let id = super::NEXT_DIALOG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        super::PENDING.lock().as_mut().unwrap().insert(
            id,
            super::Request {
                epoch: super::EPOCH.load(std::sync::atomic::Ordering::Acquire),
                cancelled: true,
                event_type: 0,
                _names: Vec::new(),
                _filters: Vec::new(),
            },
        );
        // SAFETY: cancelled callbacks return before inspecting the files array.
        unsafe {
            super::dialog_done(id as *mut std::ffi::c_void, std::ptr::null(), -1);
        }
        assert!(!super::PENDING.lock().as_ref().unwrap().contains_key(&id));
    }

    #[test]
    fn a_choice_survives_the_crossing_to_the_event_loop() {
        let chosen = DialogChoice::Chosen {
            paths: vec!["/tmp/a.png".into(), "/tmp/ü b.txt".into()],
            filter: Some(1),
        };
        assert_eq!(round_trip(chosen.clone()), chosen);
        let unfiltered = DialogChoice::Chosen {
            paths: vec!["/x".into()],
            filter: None,
        };
        assert_eq!(round_trip(unfiltered.clone()), unfiltered);
        assert_eq!(round_trip(DialogChoice::Cancelled), DialogChoice::Cancelled);
        let failed = DialogChoice::Failed {
            reason: "no portal".into(),
        };
        assert_eq!(round_trip(failed.clone()), failed);
    }
}
