// Gossamer closures windowing runs from inside event pumping: each runs
// through the dispatch of the binding call that pumps, on this thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use gos_windowing::window::{HitArea, WindowId};
use gossamer_binding::{GosError, NativeDispatch, PersistentCallback, Value, register_module};

use crate::state::{with_app, with_event_app};
use crate::wire;
use crate::{fail, window};

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
// How many hooks `CALLBACKS` holds, which a plain pump reads to know it may
// run without a dispatch. Hooks are set and read on the session's thread.
static HOOKS: AtomicUsize = AtomicUsize::new(0);

// Sets or clears the hook in `slot`.
fn set_hook(slot: &'static str, hook: Option<PersistentCallback>) {
    CALLBACKS.with(|c| {
        let mut hooks = c.borrow_mut();
        match hook {
            Some(hook) => hooks.insert(slot, hook),
            None => hooks.remove(slot),
        };
        HOOKS.store(hooks.len(), Ordering::Relaxed);
    });
}

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

// The tag of the next event of the delivered batch, `wire::NO_EVENT` past it.
fn pump() -> i64 {
    with_event_app(|app| Ok(app.poll().map_or(wire::NO_EVENT, |e| wire::answer(&e))))
        .unwrap_or(wire::NO_EVENT)
}

register_module!(
    name: windowing_hooks,
    doc: "Event pumping that runs Gossamer hooks.",

    cb_fn wait(d,) -> Result<i64, GosError> {
        let _scope = Scope::enter(d);
        with_event_app(|app| app.wait().map(|e| wire::answer(&e)).map_err(fail))
    }

    cb_fn wait_timeout(d, ms: i64) -> Result<i64, GosError> {
        let _scope = Scope::enter(d);
        with_event_app(|app| app.wait_timeout(ms).map(|e| e.map_or(wire::NO_EVENT, |e| wire::answer(&e))).map_err(fail))
    }

    // A pump runs Gossamer only through a hook, so with none set `poll`
    // needs no dispatch and stays a plain call; with one set it answers
    // `wire::HOOKED` and the caller pumps through `poll_with_hooks`.
    fn poll() -> i64 {
        if HOOKS.load(Ordering::Relaxed) != 0 {
            return wire::HOOKED;
        }
        pump()
    }

    cb_fn poll_with_hooks(d,) -> i64 {
        let _scope = Scope::enter(d);
        pump()
    }

    /// Word `i` of the event a pump last answered the tag of.
    fn event_word(i: i64) -> i64 {
        wire::word(i)
    }

    fn event_texts() -> Vec<String> {
        wire::take_texts()
    }

    cb_fn on_live_resize(d, redraw: PersistentCallback) -> Result<(), GosError> {
        let _ = d;
        set_hook("live_resize", Some(redraw));
        with_app(|app| {
            app.clone().on_live_resize(Box::new(|w| {
                let _ = run("live_resize", vec![Value::Int(i64::from(w.id().0))]);
            }))
            .map_err(fail)
        })
    }

    fn clear_live_resize() -> Result<(), GosError> {
        set_hook("live_resize", None);
        with_app(|app| app.clone().clear_live_resize().map_err(fail))
    }

    cb_fn set_hit_test(d, id: i64, area: PersistentCallback) -> Result<(), GosError> {
        let _ = d;
        let w = window(id)?;
        let slot = hit_slot(w.id());
        set_hook(slot, Some(area));
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
        set_hook(hit_slot(w.id()), None);
        w.set_hit_test(None).map_err(fail)
    }
);
