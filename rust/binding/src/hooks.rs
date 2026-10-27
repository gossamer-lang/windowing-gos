// Gossamer closures windowing runs from inside event pumping: each runs
// through the dispatch of the binding call that pumps, on this thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

use gos_windowing::window::{HitArea, WindowId};
use gossamer_binding::{GosError, NativeDispatch, PersistentCallback, Value, register_module};

use crate::state::with_app;
use crate::{fail, to_json, window};

thread_local! {
    static CURRENT: Cell<Option<NonNull<dyn NativeDispatch + 'static>>> = const { Cell::new(None) };
    // Kept until cleared, never dropped with the thread: the thread is the
    // program's, and by its teardown the runtime a release returns a callback
    // to is gone.
    static CALLBACKS: ManuallyDrop<RefCell<HashMap<&'static str, PersistentCallback>>> =
        ManuallyDrop::new(RefCell::new(HashMap::new()));
}

/// Publishes a binding call's dispatch while the guard lives.
struct Scope {
    previous: Option<NonNull<dyn NativeDispatch + 'static>>,
}

impl Scope {
    fn enter(d: &mut dyn NativeDispatch) -> Scope {
        let short: NonNull<dyn NativeDispatch + '_> = NonNull::from(d);
        // SAFETY: only the lifetime bound changes. The pointer is read only
        // while this guard lives, which is inside the call that lent `d`.
        let long: NonNull<dyn NativeDispatch + 'static> = unsafe { std::mem::transmute(short) };
        Scope {
            previous: CURRENT.with(|c| c.replace(Some(long))),
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.previous));
    }
}

// Runs the closure kept under `slot`. SDL runs hooks only while a binding
// call pumps events; one running at any other time has no dispatch and does
// nothing.
fn run(slot: &'static str, args: Vec<Value>) -> Option<Value> {
    let f = CALLBACKS.with(|c| c.borrow().get(slot).cloned())?;
    let mut d = CURRENT.with(Cell::get)?;
    // SAFETY: a live `Scope` on this thread published `d`, and the pumping
    // call that lent it is still running: SDL runs hooks synchronously.
    f.invoke(unsafe { d.as_mut() }, args).ok()
}

fn hit_slot(id: WindowId) -> &'static str {
    // One slot per window; the names live as long as the process does.
    Box::leak(format!("hit:{}", id.0).into_boxed_str())
}

const HIT_AREAS: [HitArea; 10] = [
    HitArea::Normal,
    HitArea::Draggable,
    HitArea::ResizeTopLeft,
    HitArea::ResizeTop,
    HitArea::ResizeTopRight,
    HitArea::ResizeRight,
    HitArea::ResizeBottomRight,
    HitArea::ResizeBottom,
    HitArea::ResizeBottomLeft,
    HitArea::ResizeLeft,
];

register_module!(
    name: windowing_hooks,
    doc: "Event pumping that runs Gossamer hooks.",

    cb_fn wait(d,) -> Result<String, GosError> {
        let _scope = Scope::enter(d);
        with_app(|app| app.wait().map(|e| to_json(&e)).map_err(fail))
    }

    cb_fn wait_timeout(d, ms: i64) -> Result<String, GosError> {
        let _scope = Scope::enter(d);
        with_app(|app| app.wait_timeout(ms).map(|e| e.map(|e| to_json(&e)).unwrap_or_default()).map_err(fail))
    }

    cb_fn poll(d,) -> String {
        let _scope = Scope::enter(d);
        with_app(|app| Ok(app.poll().map(|e| to_json(&e)).unwrap_or_default())).unwrap_or_default()
    }

    cb_fn on_live_resize(d, redraw: PersistentCallback) -> Result<(), GosError> {
        let _ = d;
        CALLBACKS.with(|c| c.borrow_mut().insert("live_resize", redraw));
        with_app(|app| {
            app.clone().on_live_resize(Box::new(|w| {
                let _ = run("live_resize", vec![Value::Int(i64::from(w.id().0))]);
            }))
            .map_err(fail)
        })
    }

    fn clear_live_resize() -> Result<(), GosError> {
        CALLBACKS.with(|c| c.borrow_mut().remove("live_resize"));
        with_app(|app| app.clone().clear_live_resize().map_err(fail))
    }

    cb_fn set_hit_test(d, id: i64, area: PersistentCallback) -> Result<(), GosError> {
        let _ = d;
        let w = window(id)?;
        let slot = hit_slot(w.id());
        CALLBACKS.with(|c| c.borrow_mut().insert(slot, area));
        w.set_hit_test(Some(Box::new(move |x, y| {
            match run(slot, vec![Value::Float(x), Value::Float(y)]) {
                Some(Value::Int(n)) => usize::try_from(n).ok().and_then(|n| HIT_AREAS.get(n).copied()).unwrap_or(HitArea::Normal),
                _ => HitArea::Normal,
            }
        })))
        .map_err(fail)
    }

    fn clear_hit_test(id: i64) -> Result<(), GosError> {
        let w = window(id)?;
        CALLBACKS.with(|c| c.borrow_mut().remove(hit_slot(w.id())));
        w.set_hit_test(None).map_err(fail)
    }
);
