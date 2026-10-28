//! Events cross the binding as words, scalars the binding passes as they
//! are: the variant's position in `Event` as the tag, then each field as one
//! or more words, and the event's strings beside them as texts. A pump
//! answers the tag and keeps the rest for `word` and `take_texts`, so an
//! event crosses without an allocation.
//!
//! A word carries an id, a count, a boolean (0 or 1), an enum position, a key
//! or scancode code, the bit set of `Mods`, `Buttons`, or `PenState`, or the
//! bit pattern of an `f64`. `src/wire.gos` reads and writes the same layout.

use std::cell::RefCell;
use std::sync::atomic::{AtomicI64, Ordering};

use gos_windowing::{
    Buttons, DialogChoice, DialogId, DisplayId, DragId, DragOperation, Event, Key, MenuSource,
    Mods, MouseButton, Orientation, PenAxis, PenState, PointerSource, Scancode, Theme, TrayId,
    WindowId,
};

/// The most words an event takes: `TouchMoved`'s tag and eight fields.
pub const MOST_WORDS: usize = 9;

/// An event's words, the tag first, zero past its own.
pub type Words = [i64; MOST_WORDS];

/// The tag a plain pump answers in place of an event while a hook is set,
/// which only a pump that can run Gossamer may run.
pub const HOOKED: i64 = -1;

/// The tag a pump answers when no event came.
pub const NO_EVENT: i64 = -2;

// The words of the event `answer` last gave the tag of, which the Gossamer
// side reads right after through `word`. Only the thread that started the
// session pumps or reads them, so relaxed loads and stores see each other in
// program order.
static WORDS: [AtomicI64; MOST_WORDS] = [const { AtomicI64::new(0) }; MOST_WORDS];

thread_local! {
    // The texts beside those words, which `take_texts` hands over.
    static TEXTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// `event`'s tag, keeping its words for [`word`] and its texts for
/// [`take_texts`].
pub fn answer(event: &Event) -> i64 {
    let (words, texts) = encode(event);
    for (kept, word) in WORDS.iter().zip(words) {
        kept.store(word, Ordering::Relaxed);
    }
    if carries_texts(words[0]) {
        TEXTS.with(|t| *t.borrow_mut() = texts);
    }
    words[0]
}

/// Word `i` of the event [`answer`] last gave the tag of, 0 past its words.
pub fn word(i: i64) -> i64 {
    usize::try_from(i)
        .ok()
        .and_then(|i| WORDS.get(i))
        .map_or(0, |kept| kept.load(Ordering::Relaxed))
}

/// The texts of the event [`answer`] last gave the tag of, or that
/// [`keep_texts`] kept.
pub fn take_texts() -> Vec<String> {
    TEXTS.with(|t| std::mem::take(&mut *t.borrow_mut()))
}

/// Keeps `texts` for the event the next decode of words reads.
pub fn keep_texts(texts: Vec<String>) {
    TEXTS.with(|t| *t.borrow_mut() = texts);
}

/// Whether the event `tag` names has texts beside its words.
pub fn carries_texts(tag: i64) -> bool {
    matches!(tag, 14 | 42 | 43 | 65 | 66 | 68)
}

struct Writer {
    words: Words,
    len: usize,
    texts: Vec<String>,
}

impl Writer {
    fn int(&mut self, value: i64) -> &mut Self {
        self.words[self.len] = value;
        self.len += 1;
        self
    }

    fn float(&mut self, value: f64) -> &mut Self {
        self.int(value.to_bits() as i64)
    }

    fn text(&mut self, value: &str) -> &mut Self {
        self.texts.push(value.to_string());
        self
    }

    fn flag(&mut self, value: bool) -> &mut Self {
        self.int(i64::from(value))
    }

    fn window(&mut self, id: WindowId) -> &mut Self {
        self.int(i64::from(id.0))
    }

    fn display(&mut self, id: DisplayId) -> &mut Self {
        self.int(i64::from(id.0))
    }

    fn small(&mut self, value: u32) -> &mut Self {
        self.int(i64::from(value))
    }

    // u64 ids keep their bit pattern in an i64.
    fn wide(&mut self, value: u64) -> &mut Self {
        self.int(value as i64)
    }

    fn mods(&mut self, m: Mods) -> &mut Self {
        self.int(bits(&[
            m.shift,
            m.ctrl,
            m.alt,
            m.gui,
            m.alt_gr,
            m.caps_lock,
            m.num_lock,
            m.scroll_lock,
        ]))
    }

    fn buttons(&mut self, b: Buttons) -> &mut Self {
        self.int(bits(&[b.left, b.middle, b.right, b.back, b.forward]))
    }

    fn pen_state(&mut self, s: PenState) -> &mut Self {
        self.int(bits(&[
            s.down,
            s.eraser,
            s.primary_button,
            s.secondary_button,
            s.tertiary_button,
        ]))
    }

    fn source(&mut self, s: PointerSource) -> &mut Self {
        self.int(match s {
            PointerSource::Mouse => 0,
            PointerSource::Touch => 1,
            PointerSource::Pen => 2,
        })
    }

    // A kind, then the number `Other` carries (0 for the named buttons).
    fn button(&mut self, b: MouseButton) -> &mut Self {
        let (kind, n) = match b {
            MouseButton::Left => (0, 0),
            MouseButton::Middle => (1, 0),
            MouseButton::Right => (2, 0),
            MouseButton::Back => (3, 0),
            MouseButton::Forward => (4, 0),
            MouseButton::Other(n) => (5, n),
        };
        self.int(kind).int(n)
    }

    fn axis(&mut self, a: PenAxis) -> &mut Self {
        let (kind, n) = match a {
            PenAxis::Pressure => (0, 0),
            PenAxis::TiltX => (1, 0),
            PenAxis::TiltY => (2, 0),
            PenAxis::Distance => (3, 0),
            PenAxis::Rotation => (4, 0),
            PenAxis::Slider => (5, 0),
            PenAxis::TangentialPressure => (6, 0),
            PenAxis::Other(n) => (7, n),
        };
        self.int(kind).int(n)
    }

    fn pointer(&mut self, x: f64, y: f64) -> &mut Self {
        self.float(x).float(y)
    }
}

fn bits(flags: &[bool]) -> i64 {
    flags
        .iter()
        .enumerate()
        .fold(0, |acc, (i, &on)| acc | (i64::from(on) << i))
}

/// `event` as flat words.
pub fn encode(event: &Event) -> (Words, Vec<String>) {
    // The tag's word, filled once the match names the variant.
    let mut w = Writer {
        words: [0; MOST_WORDS],
        len: 1,
        texts: Vec::new(),
    };
    let tag = match event {
        Event::QuitRequested => 0,
        Event::Terminating => 1,
        Event::LowMemory => 2,
        Event::Suspending => 3,
        Event::Suspended => 4,
        Event::Resuming => 5,
        Event::Resumed => 6,
        Event::LocaleChanged => 7,
        Event::ThemeChanged { theme } => {
            w.int(match theme {
                Theme::Light => 0,
                Theme::Dark => 1,
                Theme::Unknown => 2,
            });
            8
        }
        Event::KeymapChanged => 9,
        Event::ClipboardChanged { owned } => {
            w.flag(*owned);
            10
        }
        Event::Wake => 11,
        Event::RenderReset { window } => {
            w.window(*window);
            12
        }
        Event::RedrawRequested { window } => {
            w.window(*window);
            13
        }
        Event::DialogClosed { dialog, choice } => {
            w.wide(dialog.0);
            match choice {
                DialogChoice::Chosen { paths, filter } => {
                    w.int(0).flag(filter.is_some()).int(filter.unwrap_or(0));
                    for path in paths {
                        w.text(path);
                    }
                }
                DialogChoice::Cancelled => {
                    w.int(1);
                }
                DialogChoice::Failed { reason } => {
                    w.int(2).text(reason);
                }
            }
            14
        }
        Event::DisplayAdded { display } => {
            w.display(*display);
            15
        }
        Event::DisplayRemoved { display } => {
            w.display(*display);
            16
        }
        Event::DisplayMoved { display } => {
            w.display(*display);
            17
        }
        Event::DisplayModeChanged { display } => {
            w.display(*display);
            18
        }
        Event::DisplayScaleChanged { display } => {
            w.display(*display);
            19
        }
        Event::DisplayUsableBoundsChanged { display } => {
            w.display(*display);
            20
        }
        Event::DisplayOrientationChanged {
            display,
            orientation,
        } => {
            w.display(*display).int(match orientation {
                Orientation::Unknown => 0,
                Orientation::Landscape => 1,
                Orientation::LandscapeFlipped => 2,
                Orientation::Portrait => 3,
                Orientation::PortraitFlipped => 4,
            });
            21
        }
        Event::WindowShown { window } => {
            w.window(*window);
            22
        }
        Event::WindowHidden { window } => {
            w.window(*window);
            23
        }
        Event::Moved { window, x, y } => {
            w.window(*window).int(*x).int(*y);
            24
        }
        Event::Resized {
            window,
            width,
            height,
        } => {
            w.window(*window).int(*width).int(*height);
            25
        }
        Event::PixelSizeChanged {
            window,
            width,
            height,
        } => {
            w.window(*window).int(*width).int(*height);
            26
        }
        Event::ScaleChanged { window, scale } => {
            w.window(*window).float(*scale);
            27
        }
        Event::Minimized { window } => {
            w.window(*window);
            28
        }
        Event::Maximized { window } => {
            w.window(*window);
            29
        }
        Event::Restored { window } => {
            w.window(*window);
            30
        }
        Event::PointerEntered { window } => {
            w.window(*window);
            31
        }
        Event::PointerLeft { window } => {
            w.window(*window);
            32
        }
        Event::FocusGained { window } => {
            w.window(*window);
            33
        }
        Event::FocusLost { window } => {
            w.window(*window);
            34
        }
        Event::CloseRequested { window } => {
            w.window(*window);
            35
        }
        Event::DisplayChanged { window, display } => {
            w.window(*window).display(*display);
            36
        }
        Event::Occluded { window } => {
            w.window(*window);
            37
        }
        Event::EnteredFullscreen { window } => {
            w.window(*window);
            38
        }
        Event::LeftFullscreen { window } => {
            w.window(*window);
            39
        }
        Event::KeyDown {
            window,
            key,
            scancode,
            mods,
            repeat,
        } => {
            w.window(*window)
                .small(key.code())
                .int(i64::from(scancode.code()))
                .mods(*mods)
                .flag(*repeat);
            40
        }
        Event::KeyUp {
            window,
            key,
            scancode,
            mods,
        } => {
            w.window(*window)
                .small(key.code())
                .int(i64::from(scancode.code()))
                .mods(*mods);
            41
        }
        Event::Text { window, text } => {
            w.window(*window).text(text);
            42
        }
        Event::Composing {
            window,
            text,
            cursor,
            selection,
        } => {
            w.window(*window).int(*cursor).int(*selection).text(text);
            43
        }
        Event::PointerMoved {
            window,
            x,
            y,
            dx,
            dy,
            buttons,
            source,
        } => {
            w.window(*window)
                .buttons(*buttons)
                .source(*source)
                .pointer(*x, *y)
                .float(*dx)
                .float(*dy);
            44
        }
        Event::ButtonDown {
            window,
            button,
            x,
            y,
            clicks,
            source,
        } => {
            w.window(*window)
                .button(*button)
                .int(*clicks)
                .source(*source)
                .pointer(*x, *y);
            45
        }
        Event::ButtonUp {
            window,
            button,
            x,
            y,
            clicks,
            source,
        } => {
            w.window(*window)
                .button(*button)
                .int(*clicks)
                .source(*source)
                .pointer(*x, *y);
            46
        }
        Event::Wheel {
            window,
            dx,
            dy,
            x,
            y,
        } => {
            w.window(*window).float(*dx).float(*dy).pointer(*x, *y);
            47
        }
        Event::TouchDown {
            window,
            device,
            finger,
            x,
            y,
            pressure,
        } => {
            w.window(*window)
                .wide(*device)
                .wide(*finger)
                .pointer(*x, *y)
                .float(*pressure);
            48
        }
        Event::TouchMoved {
            window,
            device,
            finger,
            x,
            y,
            dx,
            dy,
            pressure,
        } => {
            w.window(*window)
                .wide(*device)
                .wide(*finger)
                .pointer(*x, *y)
                .float(*dx)
                .float(*dy)
                .float(*pressure);
            49
        }
        Event::TouchUp {
            window,
            device,
            finger,
            x,
            y,
        } => {
            w.window(*window)
                .wide(*device)
                .wide(*finger)
                .pointer(*x, *y);
            50
        }
        Event::TouchCancelled {
            window,
            device,
            finger,
        } => {
            w.window(*window).wide(*device).wide(*finger);
            51
        }
        Event::PinchBegan { window } => {
            w.window(*window);
            52
        }
        Event::Pinch { window, scale } => {
            w.window(*window).float(*scale);
            53
        }
        Event::PinchEnded { window } => {
            w.window(*window);
            54
        }
        Event::PenEntered { window, pen } => {
            w.window(*window).small(*pen);
            55
        }
        Event::PenLeft { window, pen } => {
            w.window(*window).small(*pen);
            56
        }
        Event::PenDown {
            window,
            pen,
            x,
            y,
            eraser,
        } => {
            w.window(*window).small(*pen).flag(*eraser).pointer(*x, *y);
            57
        }
        Event::PenUp {
            window,
            pen,
            x,
            y,
            eraser,
        } => {
            w.window(*window).small(*pen).flag(*eraser).pointer(*x, *y);
            58
        }
        Event::PenMoved {
            window,
            pen,
            x,
            y,
            state,
        } => {
            w.window(*window)
                .small(*pen)
                .pen_state(*state)
                .pointer(*x, *y);
            59
        }
        Event::PenButtonDown {
            window,
            pen,
            button,
            x,
            y,
        } => {
            w.window(*window).small(*pen).int(*button).pointer(*x, *y);
            60
        }
        Event::PenButtonUp {
            window,
            pen,
            button,
            x,
            y,
        } => {
            w.window(*window).small(*pen).int(*button).pointer(*x, *y);
            61
        }
        Event::PenAxisChanged {
            window,
            pen,
            axis,
            value,
            x,
            y,
        } => {
            w.window(*window)
                .small(*pen)
                .axis(*axis)
                .float(*value)
                .pointer(*x, *y);
            62
        }
        Event::DropBegan { window } => {
            w.window(*window);
            63
        }
        Event::DropMoved { window, x, y } => {
            w.window(*window).pointer(*x, *y);
            64
        }
        Event::DroppedFile { window, path, x, y } => {
            w.window(*window).text(path).pointer(*x, *y);
            65
        }
        Event::DroppedText { window, text, x, y } => {
            w.window(*window).text(text).pointer(*x, *y);
            66
        }
        Event::DropEnded { window, x, y } => {
            w.window(*window).pointer(*x, *y);
            67
        }
        Event::OpenRequested { path } => {
            w.text(path);
            68
        }
        Event::MenuSelected { source, item } => {
            // A menu bar choice carries its window, -1 for none; a window id
            // is a u32, so -1 names no window.
            match source {
                MenuSource::MenuBar { window } => {
                    w.int(0).int(window.map_or(-1, |id| i64::from(id.0)));
                }
                MenuSource::Tray { tray } => {
                    w.int(1).int(i64::from(tray.0));
                }
            }
            w.small(*item);
            69
        }
        Event::DragFinished { drag, operation } => {
            w.wide(drag.0).int(match operation {
                None => -1,
                Some(DragOperation::Copy) => 0,
                Some(DragOperation::Move) => 1,
                Some(DragOperation::Link) => 2,
            });
            70
        }
        Event::PreferencesChanged => 71,
    };
    w.words[0] = tag;
    (w.words, w.texts)
}

struct Reader<'a> {
    words: std::slice::Iter<'a, i64>,
    texts: std::vec::IntoIter<String>,
}

impl Reader<'_> {
    fn int(&mut self) -> Result<i64, String> {
        self.words
            .next()
            .copied()
            .ok_or_else(|| "an event is missing a word".into())
    }

    fn float(&mut self) -> Result<f64, String> {
        self.int().map(|bits| f64::from_bits(bits as u64))
    }

    fn text(&mut self) -> Result<String, String> {
        self.texts
            .next()
            .ok_or_else(|| "an event is missing a text".into())
    }

    fn flag(&mut self) -> Result<bool, String> {
        Ok(self.int()? != 0)
    }

    fn small(&mut self) -> Result<u32, String> {
        let n = self.int()?;
        u32::try_from(n).map_err(|_| format!("{n} is out of range for a 32-bit id"))
    }

    fn wide(&mut self) -> Result<u64, String> {
        Ok(self.int()? as u64)
    }

    fn window(&mut self) -> Result<WindowId, String> {
        self.small().map(WindowId)
    }

    fn display(&mut self) -> Result<DisplayId, String> {
        self.small().map(DisplayId)
    }

    fn bits<const N: usize>(&mut self) -> Result<[bool; N], String> {
        let n = self.int()?;
        Ok(std::array::from_fn(|i| n & (1 << i) != 0))
    }

    fn mods(&mut self) -> Result<Mods, String> {
        let [
            shift,
            ctrl,
            alt,
            gui,
            alt_gr,
            caps_lock,
            num_lock,
            scroll_lock,
        ] = self.bits()?;
        Ok(Mods {
            shift,
            ctrl,
            alt,
            gui,
            alt_gr,
            caps_lock,
            num_lock,
            scroll_lock,
        })
    }

    fn buttons(&mut self) -> Result<Buttons, String> {
        let [left, middle, right, back, forward] = self.bits()?;
        Ok(Buttons {
            left,
            middle,
            right,
            back,
            forward,
        })
    }

    fn pen_state(&mut self) -> Result<PenState, String> {
        let [
            down,
            eraser,
            primary_button,
            secondary_button,
            tertiary_button,
        ] = self.bits()?;
        Ok(PenState {
            down,
            eraser,
            primary_button,
            secondary_button,
            tertiary_button,
        })
    }

    fn source(&mut self) -> Result<PointerSource, String> {
        match self.int()? {
            0 => Ok(PointerSource::Mouse),
            1 => Ok(PointerSource::Touch),
            2 => Ok(PointerSource::Pen),
            n => Err(format!("{n} is not a pointer source")),
        }
    }

    fn button(&mut self) -> Result<MouseButton, String> {
        let kind = self.int()?;
        let n = self.int()?;
        match kind {
            0 => Ok(MouseButton::Left),
            1 => Ok(MouseButton::Middle),
            2 => Ok(MouseButton::Right),
            3 => Ok(MouseButton::Back),
            4 => Ok(MouseButton::Forward),
            5 => Ok(MouseButton::Other(n)),
            k => Err(format!("{k} is not a mouse button")),
        }
    }

    fn axis(&mut self) -> Result<PenAxis, String> {
        let kind = self.int()?;
        let n = self.int()?;
        match kind {
            0 => Ok(PenAxis::Pressure),
            1 => Ok(PenAxis::TiltX),
            2 => Ok(PenAxis::TiltY),
            3 => Ok(PenAxis::Distance),
            4 => Ok(PenAxis::Rotation),
            5 => Ok(PenAxis::Slider),
            6 => Ok(PenAxis::TangentialPressure),
            7 => Ok(PenAxis::Other(n)),
            k => Err(format!("{k} is not a pen axis")),
        }
    }

    fn key(&mut self) -> Result<Key, String> {
        self.small().map(Key::from_code)
    }

    fn scancode(&mut self) -> Result<Scancode, String> {
        let n = self.int()?;
        i32::try_from(n)
            .map(Scancode::from_code)
            .map_err(|_| format!("{n} is not a scancode"))
    }

    fn choice(&mut self) -> Result<DialogChoice, String> {
        match self.int()? {
            0 => {
                let has_filter = self.flag()?;
                let filter = self.int()?;
                Ok(DialogChoice::Chosen {
                    paths: self.texts.by_ref().collect(),
                    filter: has_filter.then_some(filter),
                })
            }
            1 => Ok(DialogChoice::Cancelled),
            2 => Ok(DialogChoice::Failed {
                reason: self.text()?,
            }),
            n => Err(format!("{n} is not a dialog choice")),
        }
    }
}

/// The event `words` and `texts` name; words past the event's own are
/// ignored.
pub fn decode(words: &[i64], texts: Vec<String>) -> Result<Event, String> {
    let mut r = Reader {
        words: words.iter(),
        texts: texts.into_iter(),
    };
    let tag = r.int()?;
    Ok(match tag {
        0 => Event::QuitRequested,
        1 => Event::Terminating,
        2 => Event::LowMemory,
        3 => Event::Suspending,
        4 => Event::Suspended,
        5 => Event::Resuming,
        6 => Event::Resumed,
        7 => Event::LocaleChanged,
        8 => Event::ThemeChanged {
            theme: match r.int()? {
                0 => Theme::Light,
                1 => Theme::Dark,
                2 => Theme::Unknown,
                n => return Err(format!("{n} is not a theme")),
            },
        },
        9 => Event::KeymapChanged,
        10 => Event::ClipboardChanged { owned: r.flag()? },
        11 => Event::Wake,
        12 => Event::RenderReset {
            window: r.window()?,
        },
        13 => Event::RedrawRequested {
            window: r.window()?,
        },
        14 => Event::DialogClosed {
            dialog: DialogId(r.wide()?),
            choice: r.choice()?,
        },
        15 => Event::DisplayAdded {
            display: r.display()?,
        },
        16 => Event::DisplayRemoved {
            display: r.display()?,
        },
        17 => Event::DisplayMoved {
            display: r.display()?,
        },
        18 => Event::DisplayModeChanged {
            display: r.display()?,
        },
        19 => Event::DisplayScaleChanged {
            display: r.display()?,
        },
        20 => Event::DisplayUsableBoundsChanged {
            display: r.display()?,
        },
        21 => Event::DisplayOrientationChanged {
            display: r.display()?,
            orientation: match r.int()? {
                0 => Orientation::Unknown,
                1 => Orientation::Landscape,
                2 => Orientation::LandscapeFlipped,
                3 => Orientation::Portrait,
                4 => Orientation::PortraitFlipped,
                n => return Err(format!("{n} is not an orientation")),
            },
        },
        22 => Event::WindowShown {
            window: r.window()?,
        },
        23 => Event::WindowHidden {
            window: r.window()?,
        },
        24 => Event::Moved {
            window: r.window()?,
            x: r.int()?,
            y: r.int()?,
        },
        25 => Event::Resized {
            window: r.window()?,
            width: r.int()?,
            height: r.int()?,
        },
        26 => Event::PixelSizeChanged {
            window: r.window()?,
            width: r.int()?,
            height: r.int()?,
        },
        27 => Event::ScaleChanged {
            window: r.window()?,
            scale: r.float()?,
        },
        28 => Event::Minimized {
            window: r.window()?,
        },
        29 => Event::Maximized {
            window: r.window()?,
        },
        30 => Event::Restored {
            window: r.window()?,
        },
        31 => Event::PointerEntered {
            window: r.window()?,
        },
        32 => Event::PointerLeft {
            window: r.window()?,
        },
        33 => Event::FocusGained {
            window: r.window()?,
        },
        34 => Event::FocusLost {
            window: r.window()?,
        },
        35 => Event::CloseRequested {
            window: r.window()?,
        },
        36 => Event::DisplayChanged {
            window: r.window()?,
            display: r.display()?,
        },
        37 => Event::Occluded {
            window: r.window()?,
        },
        38 => Event::EnteredFullscreen {
            window: r.window()?,
        },
        39 => Event::LeftFullscreen {
            window: r.window()?,
        },
        40 => Event::KeyDown {
            window: r.window()?,
            key: r.key()?,
            scancode: r.scancode()?,
            mods: r.mods()?,
            repeat: r.flag()?,
        },
        41 => Event::KeyUp {
            window: r.window()?,
            key: r.key()?,
            scancode: r.scancode()?,
            mods: r.mods()?,
        },
        42 => Event::Text {
            window: r.window()?,
            text: r.text()?,
        },
        43 => Event::Composing {
            window: r.window()?,
            cursor: r.int()?,
            selection: r.int()?,
            text: r.text()?,
        },
        44 => Event::PointerMoved {
            window: r.window()?,
            buttons: r.buttons()?,
            source: r.source()?,
            x: r.float()?,
            y: r.float()?,
            dx: r.float()?,
            dy: r.float()?,
        },
        45 => Event::ButtonDown {
            window: r.window()?,
            button: r.button()?,
            clicks: r.int()?,
            source: r.source()?,
            x: r.float()?,
            y: r.float()?,
        },
        46 => Event::ButtonUp {
            window: r.window()?,
            button: r.button()?,
            clicks: r.int()?,
            source: r.source()?,
            x: r.float()?,
            y: r.float()?,
        },
        47 => Event::Wheel {
            window: r.window()?,
            dx: r.float()?,
            dy: r.float()?,
            x: r.float()?,
            y: r.float()?,
        },
        48 => Event::TouchDown {
            window: r.window()?,
            device: r.wide()?,
            finger: r.wide()?,
            x: r.float()?,
            y: r.float()?,
            pressure: r.float()?,
        },
        49 => Event::TouchMoved {
            window: r.window()?,
            device: r.wide()?,
            finger: r.wide()?,
            x: r.float()?,
            y: r.float()?,
            dx: r.float()?,
            dy: r.float()?,
            pressure: r.float()?,
        },
        50 => Event::TouchUp {
            window: r.window()?,
            device: r.wide()?,
            finger: r.wide()?,
            x: r.float()?,
            y: r.float()?,
        },
        51 => Event::TouchCancelled {
            window: r.window()?,
            device: r.wide()?,
            finger: r.wide()?,
        },
        52 => Event::PinchBegan {
            window: r.window()?,
        },
        53 => Event::Pinch {
            window: r.window()?,
            scale: r.float()?,
        },
        54 => Event::PinchEnded {
            window: r.window()?,
        },
        55 => Event::PenEntered {
            window: r.window()?,
            pen: r.small()?,
        },
        56 => Event::PenLeft {
            window: r.window()?,
            pen: r.small()?,
        },
        57 => Event::PenDown {
            window: r.window()?,
            pen: r.small()?,
            eraser: r.flag()?,
            x: r.float()?,
            y: r.float()?,
        },
        58 => Event::PenUp {
            window: r.window()?,
            pen: r.small()?,
            eraser: r.flag()?,
            x: r.float()?,
            y: r.float()?,
        },
        59 => Event::PenMoved {
            window: r.window()?,
            pen: r.small()?,
            state: r.pen_state()?,
            x: r.float()?,
            y: r.float()?,
        },
        60 => Event::PenButtonDown {
            window: r.window()?,
            pen: r.small()?,
            button: r.int()?,
            x: r.float()?,
            y: r.float()?,
        },
        61 => Event::PenButtonUp {
            window: r.window()?,
            pen: r.small()?,
            button: r.int()?,
            x: r.float()?,
            y: r.float()?,
        },
        62 => Event::PenAxisChanged {
            window: r.window()?,
            pen: r.small()?,
            axis: r.axis()?,
            value: r.float()?,
            x: r.float()?,
            y: r.float()?,
        },
        63 => Event::DropBegan {
            window: r.window()?,
        },
        64 => Event::DropMoved {
            window: r.window()?,
            x: r.float()?,
            y: r.float()?,
        },
        65 => Event::DroppedFile {
            window: r.window()?,
            path: r.text()?,
            x: r.float()?,
            y: r.float()?,
        },
        66 => Event::DroppedText {
            window: r.window()?,
            text: r.text()?,
            x: r.float()?,
            y: r.float()?,
        },
        67 => Event::DropEnded {
            window: r.window()?,
            x: r.float()?,
            y: r.float()?,
        },
        68 => Event::OpenRequested { path: r.text()? },
        69 => {
            let source = match r.int()? {
                0 => {
                    let window = r.int()?;
                    MenuSource::MenuBar {
                        window: (window >= 0)
                            .then(|| u32::try_from(window).map(WindowId))
                            .transpose()
                            .map_err(|_| format!("{window} is not a window id"))?,
                    }
                }
                1 => MenuSource::Tray {
                    tray: TrayId(r.small()?),
                },
                n => return Err(format!("{n} is not a menu source")),
            };
            Event::MenuSelected {
                source,
                item: r.small()?,
            }
        }
        70 => Event::DragFinished {
            drag: DragId(r.wide()?),
            operation: match r.int()? {
                -1 => None,
                0 => Some(DragOperation::Copy),
                1 => Some(DragOperation::Move),
                2 => Some(DragOperation::Link),
                n => return Err(format!("{n} is not a drag operation")),
            },
        },
        71 => Event::PreferencesChanged,
        n => return Err(format!("{n} is not an event")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One event of every variant, with every nested kind represented.
    fn samples() -> Vec<Event> {
        let window = WindowId(7);
        let mods = Mods {
            shift: true,
            alt_gr: true,
            scroll_lock: true,
            ..Mods::default()
        };
        vec![
            Event::QuitRequested,
            Event::Terminating,
            Event::LowMemory,
            Event::Suspending,
            Event::Suspended,
            Event::Resuming,
            Event::Resumed,
            Event::LocaleChanged,
            Event::ThemeChanged { theme: Theme::Dark },
            Event::KeymapChanged,
            Event::ClipboardChanged { owned: true },
            Event::Wake,
            Event::RenderReset { window },
            Event::RedrawRequested { window },
            Event::DialogClosed {
                dialog: DialogId(u64::MAX),
                choice: DialogChoice::Chosen {
                    paths: vec!["/a b".into(), "/c".into()],
                    filter: Some(-3),
                },
            },
            Event::DisplayAdded {
                display: DisplayId(2),
            },
            Event::DisplayRemoved {
                display: DisplayId(2),
            },
            Event::DisplayMoved {
                display: DisplayId(2),
            },
            Event::DisplayModeChanged {
                display: DisplayId(2),
            },
            Event::DisplayScaleChanged {
                display: DisplayId(2),
            },
            Event::DisplayUsableBoundsChanged {
                display: DisplayId(2),
            },
            Event::DisplayOrientationChanged {
                display: DisplayId(2),
                orientation: Orientation::PortraitFlipped,
            },
            Event::WindowShown { window },
            Event::WindowHidden { window },
            Event::Moved {
                window,
                x: -10,
                y: 20,
            },
            Event::Resized {
                window,
                width: 640,
                height: 480,
            },
            Event::PixelSizeChanged {
                window,
                width: 1280,
                height: 960,
            },
            Event::ScaleChanged { window, scale: 1.5 },
            Event::Minimized { window },
            Event::Maximized { window },
            Event::Restored { window },
            Event::PointerEntered { window },
            Event::PointerLeft { window },
            Event::FocusGained { window },
            Event::FocusLost { window },
            Event::CloseRequested { window },
            Event::DisplayChanged {
                window,
                display: DisplayId(3),
            },
            Event::Occluded { window },
            Event::EnteredFullscreen { window },
            Event::LeftFullscreen { window },
            Event::KeyDown {
                window,
                key: Key::A,
                scancode: Scancode::A,
                mods,
                repeat: true,
            },
            Event::KeyUp {
                window,
                key: Key::Escape,
                scancode: Scancode::Escape,
                mods: Mods::default(),
            },
            Event::Text {
                window,
                text: "héllo".into(),
            },
            Event::Composing {
                window,
                text: "かな".into(),
                cursor: 1,
                selection: 1,
            },
            Event::PointerMoved {
                window,
                x: 1.25,
                y: 2.5,
                dx: -0.5,
                dy: 0.75,
                buttons: Buttons {
                    left: true,
                    forward: true,
                    ..Buttons::default()
                },
                source: PointerSource::Touch,
            },
            Event::ButtonDown {
                window,
                button: MouseButton::Other(9),
                x: 3.0,
                y: 4.0,
                clicks: 2,
                source: PointerSource::Pen,
            },
            Event::ButtonUp {
                window,
                button: MouseButton::Back,
                x: 3.0,
                y: 4.0,
                clicks: 1,
                source: PointerSource::Mouse,
            },
            Event::Wheel {
                window,
                dx: 0.5,
                dy: -1.0,
                x: 5.0,
                y: 6.0,
            },
            Event::TouchDown {
                window,
                device: u64::MAX - 1,
                finger: 3,
                x: 0.1,
                y: 0.2,
                pressure: 0.3,
            },
            Event::TouchMoved {
                window,
                device: 1,
                finger: 3,
                x: 0.1,
                y: 0.2,
                dx: 0.01,
                dy: 0.02,
                pressure: 0.4,
            },
            Event::TouchUp {
                window,
                device: 1,
                finger: 3,
                x: 0.1,
                y: 0.2,
            },
            Event::TouchCancelled {
                window,
                device: 1,
                finger: 3,
            },
            Event::PinchBegan { window },
            Event::Pinch { window, scale: 0.8 },
            Event::PinchEnded { window },
            Event::PenEntered { window, pen: 4 },
            Event::PenLeft { window, pen: 4 },
            Event::PenDown {
                window,
                pen: 4,
                x: 1.0,
                y: 2.0,
                eraser: true,
            },
            Event::PenUp {
                window,
                pen: 4,
                x: 1.0,
                y: 2.0,
                eraser: false,
            },
            Event::PenMoved {
                window,
                pen: 4,
                x: 1.0,
                y: 2.0,
                state: PenState {
                    down: true,
                    tertiary_button: true,
                    ..PenState::default()
                },
            },
            Event::PenButtonDown {
                window,
                pen: 4,
                button: 2,
                x: 1.0,
                y: 2.0,
            },
            Event::PenButtonUp {
                window,
                pen: 4,
                button: 2,
                x: 1.0,
                y: 2.0,
            },
            Event::PenAxisChanged {
                window,
                pen: 4,
                axis: PenAxis::Other(12),
                value: 0.5,
                x: 1.0,
                y: 2.0,
            },
            Event::DropBegan { window },
            Event::DropMoved {
                window,
                x: 1.0,
                y: 2.0,
            },
            Event::DroppedFile {
                window,
                path: "/tmp/x".into(),
                x: 1.0,
                y: 2.0,
            },
            Event::DroppedText {
                window,
                text: "drag".into(),
                x: 1.0,
                y: 2.0,
            },
            Event::DropEnded {
                window,
                x: 1.0,
                y: 2.0,
            },
            Event::OpenRequested {
                path: "/doc".into(),
            },
            Event::MenuSelected {
                source: MenuSource::MenuBar { window: None },
                item: 5,
            },
            Event::DragFinished {
                drag: DragId(9),
                operation: Some(DragOperation::Link),
            },
            Event::PreferencesChanged,
        ]
    }

    #[test]
    fn every_event_survives_its_wire() {
        let samples = samples();
        let tags: Vec<i64> = samples.iter().map(|e| encode(e).0[0]).collect();
        assert_eq!(tags, (0..72).collect::<Vec<i64>>());
        for event in samples {
            let (words, texts) = encode(&event);
            assert_eq!(carries_texts(words[0]), !texts.is_empty() || words[0] == 14);
            assert_eq!(decode(&words, texts), Ok(event));
        }
    }

    #[test]
    fn nested_kinds_survive_the_wire() {
        let others = [
            Event::DialogClosed {
                dialog: DialogId(1),
                choice: DialogChoice::Cancelled,
            },
            Event::DialogClosed {
                dialog: DialogId(1),
                choice: DialogChoice::Failed {
                    reason: "no portal".into(),
                },
            },
            Event::DialogClosed {
                dialog: DialogId(1),
                choice: DialogChoice::Chosen {
                    paths: Vec::new(),
                    filter: None,
                },
            },
            Event::MenuSelected {
                source: MenuSource::MenuBar {
                    window: Some(WindowId(3)),
                },
                item: 1,
            },
            Event::MenuSelected {
                source: MenuSource::Tray { tray: TrayId(2) },
                item: 1,
            },
            Event::DragFinished {
                drag: DragId(1),
                operation: None,
            },
        ];
        for event in others {
            let (words, texts) = encode(&event);
            assert_eq!(decode(&words, texts), Ok(event));
        }
    }

    #[test]
    fn a_malformed_wire_is_an_error() {
        assert!(decode(&[72], Vec::new()).is_err());
        assert!(decode(&[40, 1], Vec::new()).is_err());
        assert!(decode(&[], Vec::new()).is_err());
        assert!(decode(&[42, 1], Vec::new()).is_err());
    }
}
