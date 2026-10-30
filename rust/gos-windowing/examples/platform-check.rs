//! Real-driver regression, without injected events. Capability output is an
//! inventory; each PASS below names an operation actually exercised here.
use gos_windowing::{self as windowing, InitOptions, Rect, Support, TextInputOptions};
use std::time::{Duration, Instant};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn pump(app: &windowing::App, duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        app.wait_timeout(5)?;
    }
    Ok(())
}
fn run() -> Result<()> {
    let app = windowing::init(
        "Platform acceptance",
        &InitOptions {
            app_id: "dev.gossamer.platform-check".into(),
            ..Default::default()
        },
    )?;
    let answer = (|| -> Result<()> {
        let caps = windowing::capabilities();
        println!("capabilities: {}", serde_json::to_string(&caps)?);
        assert!(matches!(
            caps.driver.as_str(),
            "x11" | "wayland" | "windows" | "cocoa"
        ));
        let displays = windowing::display::all()?;
        assert!(!displays.is_empty());
        for display in displays {
            assert!(display.scale.is_finite() && display.scale > 0.0);
            assert!(display.bounds.width > 0);
        }
        println!("PASS displays and valid scale");
        for iteration in 0..12 {
            let window = app.window("Native Unicode שלום 👩‍💻", 320, 200, true)?;
            let id = window.id();
            pump(&app, Duration::from_millis(40))?;
            window.set_title("Changed שלום")?;
            assert_eq!(window.title(), "Changed שלום");
            assert!(window.scale().is_finite() && window.scale() > 0.0);
            window.raw_handle()?;
            window.start_text_input_with(&TextInputOptions::default())?;
            assert!(window.is_text_input_active());
            window.set_text_input_area(
                Rect {
                    x: 8,
                    y: 8,
                    width: 100,
                    height: 24,
                },
                12,
            )?;
            window.stop_text_input()?;
            assert!(!window.is_text_input_active());
            let mut old = window.framebuffer()?;
            old.fill(30, 70, 140);
            window.present(&old)?;
            let captured = window.capture()?;
            let (r, g, b) = captured.format.rgb_offsets();
            assert_eq!(
                [captured.pixels[r], captured.pixels[g], captured.pixels[b]],
                [30, 70, 140]
            );
            // Each quadrant repainted alone, a frame apiece: the window shows
            // all four, so every frame carried the others' pixels forward.
            let (w, h) = (captured.width, captured.height);
            let colors = [[200, 10, 10], [10, 200, 10], [10, 10, 200], [200, 200, 10]];
            let quadrant = |i: i64| Rect {
                x: (i % 2) * (w / 2),
                y: (i / 2) * (h / 2),
                width: w / 2,
                height: h / 2,
            };
            for (i, color) in (0..).zip(colors) {
                let q = quadrant(i);
                window.paint_surface(&[q], |pixels, _, _, stride, format| {
                    let (r, g, b) = format.rgb_offsets();
                    for y in q.y..q.y + q.height {
                        for x in q.x..q.x + q.width {
                            let at = (y * stride + x * 4) as usize;
                            pixels[at + r] = color[0];
                            pixels[at + g] = color[1];
                            pixels[at + b] = color[2];
                        }
                    }
                })?;
            }
            let shown = window.capture()?;
            let (r, g, b) = shown.format.rgb_offsets();
            for (i, color) in (0..).zip(colors) {
                let q = quadrant(i);
                let at = ((q.y + q.height / 2) * shown.stride + (q.x + q.width / 2) * 4) as usize;
                assert_eq!(
                    [
                        shown.pixels[at + r],
                        shown.pixels[at + g],
                        shown.pixels[at + b]
                    ],
                    color,
                    "quadrant {i} after four partial repaints"
                );
            }
            window.set_size(400, 240)?;
            let until = Instant::now() + Duration::from_secs(5);
            while window.size() != (400, 240) && Instant::now() < until {
                let _ = window.sync();
                pump(&app, Duration::from_millis(10))?;
            }
            assert_eq!(window.size(), (400, 240));
            assert!(
                window.present(&old).is_err(),
                "old framebuffer accepted after resize"
            );
            let mut frame = window.framebuffer()?;
            frame.fill(80, 120, 160);
            window.present(&frame)?;
            if iteration == 0 {
                if caps.get("frame-theme").unwrap().support == Support::Unavailable {
                    assert!(
                        window
                            .set_frame_theme(Some(windowing::Theme::Dark))
                            .is_err()
                    );
                }
                if caps.get("backdrop").unwrap().support == Support::Unavailable {
                    assert!(window.set_backdrop(windowing::Backdrop::Mica).is_err());
                }
                let cursor = windowing::cursor::custom(&[255; 16 * 16 * 4], 16, 16, 0, 0)?;
                cursor.show()?;
                cursor.destroy();
                windowing::clipboard::set_text("clipboard שלום 👩‍💻")?;
                assert_eq!(windowing::clipboard::text()?, "clipboard שלום 👩‍💻");
                windowing::clipboard::set_data(vec![(
                    "application/x-gossamer-check".into(),
                    vec![0, 1, 255, 0],
                )])?;
                assert_eq!(
                    windowing::clipboard::data("application/x-gossamer-check")?,
                    Some(vec![0, 1, 255, 0])
                );
                windowing::clipboard::clear()?;
                println!(
                    "PASS text-input lifecycle/caret, native handles, exact framebuffer, clipboard MIME and custom cursor"
                );
            }
            window.hide()?;
            assert!(!window.is_visible());
            window.show()?;
            window.close();
            assert!(app.window_by_id(id).is_none());
        }
        println!("PASS 12 create/resize/stale-frame/hide/show/destroy lifetimes");
        Ok(())
    })();
    app.finalize();
    answer
}
fn main() {
    if let Err(error) = run() {
        eprintln!("platform-check: {error}");
        std::process::exit(1);
    }
}
