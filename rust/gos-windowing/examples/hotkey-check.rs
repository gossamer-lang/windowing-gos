//! Global shortcuts on a real X server, typed through XTEST: each press of a
//! registered chord arrives once, including the press after one whose window
//! went away while the key was down, and after a chord whose modifier was let
//! go first.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[cfg(free_unix)]
mod keyboard {
    use std::ffi::c_int;

    use x11_dl::xlib::{self, Display, Xlib};
    // x11-dl names its libXtst loader Xf86vmode.
    use x11_dl::xtest::Xf86vmode as XTest;

    /// Types on the X server through a connection of its own, as a keyboard would.
    pub struct Keyboard {
        x: Xlib,
        test: XTest,
        display: *mut Display,
    }

    pub const F8: u64 = 0xFFC5;
    pub const F9: u64 = 0xFFC6;
    pub const CONTROL: u64 = 0xFFE3;

    impl Keyboard {
        pub fn open() -> super::Result<Self> {
            let x = Xlib::open()?;
            let test = XTest::open()?;
            // SAFETY: a null name opens $DISPLAY.
            let display = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
            if display.is_null() {
                return Err("no X display".into());
            }
            Ok(Self { x, test, display })
        }

        fn send(&self, keysym: u64, down: bool) {
            // SAFETY: display is this connection's own and live.
            unsafe {
                let code = (self.x.XKeysymToKeycode)(self.display, keysym);
                (self.test.XTestFakeKeyEvent)(self.display, u32::from(code), c_int::from(down), 0);
                (self.x.XSync)(self.display, 0);
            }
        }

        pub fn press(&self, keysym: u64) {
            self.send(keysym, true)
        }

        pub fn release(&self, keysym: u64) {
            self.send(keysym, false)
        }

        /// Gives `window` the keyboard focus, as a window manager would.
        pub fn focus(&self, window: u64) {
            // SAFETY: display is live; a window id is a plain number.
            unsafe {
                (self.x.XSetInputFocus)(
                    self.display,
                    window,
                    xlib::RevertToPointerRoot,
                    xlib::CurrentTime,
                );
                (self.x.XSync)(self.display, 0);
            }
        }
    }

    impl Drop for Keyboard {
        fn drop(&mut self) {
            // SAFETY: the connection is this value's and closes once.
            unsafe { (self.x.XCloseDisplay)(self.display) };
        }
    }
}

#[cfg(free_unix)]
mod check {
    use std::time::{Duration, Instant};

    use gos_windowing::{self as windowing, Event, HotkeyId, InitOptions, Key, Mods};

    use super::Result;
    use super::keyboard::{CONTROL, F8, F9, Keyboard};

    /// Waits up to `limit` for an event `wanted` accepts, answering whether one came.
    fn until(
        app: &windowing::App,
        limit: Duration,
        wanted: impl Fn(&Event) -> bool,
    ) -> Result<bool> {
        let end = Instant::now() + limit;
        while Instant::now() < end {
            if let Some(e) = app.wait_timeout(10)?
                && wanted(&e)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn pressed(id: HotkeyId) -> impl Fn(&Event) -> bool {
        move |e| matches!(e, Event::Hotkey { hotkey } if *hotkey == id)
    }

    pub fn run() -> Result<()> {
        let app = windowing::init(
            "Hotkey acceptance",
            &InitOptions {
                app_id: "dev.gossamer.hotkey-check".into(),
                ..Default::default()
            },
        )?;
        if windowing::capabilities().driver != "x11" {
            println!("SKIP global shortcuts need the X11 driver");
            return Ok(());
        }
        let keys = Keyboard::open()?;
        let window = app.window("Hotkeys", 320, 200, true)?;
        let windowing::RawHandle::Xlib { window: xid, .. } = window.raw_handle()? else {
            return Err("an X11 window has an Xlib handle".into());
        };
        let wait = Duration::from_secs(5);

        let f8 = app.register_hotkey(Key::F8, Mods::default(), "Hide the window")?;
        keys.focus(xid);
        assert!(
            until(&app, wait, |e| matches!(e, Event::FocusGained { .. }))?,
            "the window takes focus"
        );
        keys.press(F8);
        assert!(until(&app, wait, pressed(f8))?, "F8 arrives");
        // The window gives up focus while F8 is down, and SDL gives the keyboard
        // back as it does: the release goes to whichever window has focus now.
        window.hide()?;
        assert!(
            until(&app, wait, |e| matches!(e, Event::FocusLost { .. }))?,
            "the hidden window loses focus"
        );
        keys.release(F8);
        keys.press(F8);
        keys.release(F8);
        assert!(
            until(&app, wait, pressed(f8))?,
            "F8 pressed after its window went away arrives"
        );
        println!("PASS a press after the window hid during the last one");

        let chord = app.register_hotkey(
            Key::F9,
            Mods {
                ctrl: true,
                ..Default::default()
            },
            "Chord",
        )?;
        for round in 0..2 {
            keys.press(CONTROL);
            keys.press(F9);
            keys.release(CONTROL);
            keys.release(F9);
            assert!(
                until(&app, wait, pressed(chord))?,
                "Ctrl+F9 round {round} arrives"
            );
        }
        println!("PASS a chord whose modifier is let go first");

        app.unregister_hotkey(f8);
        app.unregister_hotkey(chord);
        Ok(())
    }
}

#[cfg(free_unix)]
use check::run;

#[cfg(not(free_unix))]
fn run() -> Result<()> {
    println!("SKIP XTEST typing is for X11");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("FAIL {e}");
        std::process::exit(1);
    }
}
