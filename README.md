# windowing-gos

[![CI](https://github.com/gossamer-lang/windowing-gos/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/gossamer-lang/windowing-gos/actions/workflows/ci.yml)

Native windows, input and desktop services for [Gossamer](https://github.com/gossamer-lang/gossamer),
on Linux (X11 and Wayland), macOS and Windows. It is the layer GUI toolkits,
games and GPU code build on: one event loop, a software framebuffer, native
handles for OpenGL, Vulkan and Metal, keyboard, IME, pointer, touch and pen
input, and the clipboard, dialogs, tray, notifications, drag and drop and
single-instance services.

Programs use it as `use windowing`. Underneath is a Rust crate on SDL 3,
which is built and linked into your program, so users install no SDL runtime.

```gossamer
use std::{errors, process}
use windowing
use windowing::{Event, Key}

fn run() -> Result<(), errors::Error> {
    let app = windowing::init("Hello", app_id: "dev.example.hello")?
    defer app.finalize()

    let win = app.window(title: "Hello", width: 900, height: 600, resizable: true)?
    loop {
        match app.wait()? {
            Event::CloseRequested { .. } | Event::KeyDown { key: Key::Escape, .. } => break
            Event::RedrawRequested { .. } | Event::PixelSizeChanged { .. } => {
                let mut frame = win.framebuffer()?
                frame.fill(30, 60, 160)
                win.present(frame)?
            }
            _ => ()
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln(f"hello: {e}")
        process::exit(1)
    }
}
```

## Install

```toml
# project.toml
[dependencies]
"github.com/gossamer-lang/windowing-gos" = { git = "https://github.com/gossamer-lang/windowing-gos", tag = "v0.1.0" }
```

Then run `gos fetch`. You need Gossamer 0.75.7 or later, a Rust toolchain, CMake and a
C compiler.
The first build compiles SDL once; later builds reuse it. On Linux, install
the X11 and Wayland development headers first:

```sh
sudo apt-get install cmake ninja-build libx11-dev libxext-dev libxrandr-dev \
  libxcursor-dev libxi-dev libxss-dev libxfixes-dev libxtst-dev libxkbcommon-dev \
  libwayland-dev wayland-protocols libegl-dev libdecor-0-dev
```

## What it covers

- **Event loop:** `app.wait()`, `wait_timeout` and `poll`; a `Waker` lets
  goroutines wake the loop.
- **Windows:** sizes in logical units, fullscreen, borderless hit testing,
  opacity, parents and icons.
- **Drawing:** a software framebuffer you fill and present, or native handles
  and contexts for OpenGL, Vulkan and Metal.
- **Input:** layout-aware keys and physical scancodes, text and IME
  composition, pointer, touch, pinch and pen.
- **Services:** clipboard, file dialogs and message boxes, cursors, displays,
  theme and desktop preferences, tray, notifications, outbound drag and drop,
  screen-reader providers on Windows and macOS, and single instance.

## Documentation

- [Guide](https://gossamer-lang.org/docs/ecosystem/windowing/guide/): the event loop, windows, drawing, input and every
  service, with the differences between platforms.
- [Platform capabilities](https://gossamer-lang.org/docs/ecosystem/windowing/platform-capabilities/): what each driver
  supports.
- [Testing](https://gossamer-lang.org/docs/ecosystem/windowing/testing/): the test suites and how to run them.
- `examples/`: `hello`, `events`, `ticker`, `tray` and `single-instance`.

## Testing

```sh
./quick-check.sh        # format, lint and type-check
tests/parity/run.sh     # the fixtures on the VM, the JIT and both native profiles
```

See [Testing](https://gossamer-lang.org/docs/ecosystem/windowing/testing/) for the real-display, drag and drop and
single-instance suites, and the Rust checks.

## License

Apache 2.0. SDL is under the zlib license; the build compiles it from source
through `sdl3-sys` 0.6.6. `vendor/SDL` holds SDL 3.4.10's keycode and scancode
headers, which `tools/gen_keys.gos` generates `src/keys.gos` from, and SDL's
license.
