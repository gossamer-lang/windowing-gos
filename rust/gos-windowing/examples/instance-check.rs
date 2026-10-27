//! Two real windows in separate processes verify forwarding and actual focus.
use gos_windowing::{
    App, InitOptions, Window,
    instance::{self, Claim, Request},
};
use std::{
    error::Error,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn uris() -> Vec<String> {
    vec![
        instance::file_uri(
            &std::env::temp_dir()
                .join("instance שלום 👩‍💻.txt")
                .to_string_lossy(),
        ),
        "custom:activation".into(),
    ]
}
fn show(app: &App, title: &str) -> Result<Window> {
    let window = app.window(title, 320, 200, true)?;
    let mut frame = window.framebuffer()?;
    frame.fill(40, 100, 180);
    window.present(&frame)?;
    Ok(window)
}
fn pump(app: &App, duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        app.wait_timeout(10)?;
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn cpu_ns() -> Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time is writable storage for this process's CPU clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64)
}

fn child(app: &App, id: &str, done: &Path) -> Result<()> {
    let window = show(app, "Secondary launcher")?;
    window.focus()?;
    pump(app, Duration::from_millis(200))?;
    #[cfg(target_os = "linux")]
    {
        use std::collections::HashMap;
        use zbus::zvariant::Value;
        let token = window.activation_token()?;
        let mut platform = HashMap::<&str, Value<'_>>::new();
        platform.insert("activation-token", Value::from(token.as_str()));
        let startup = format!(
            "native-launch_TIME{}",
            std::env::var("NATIVE_INSTANCE_TIME").unwrap_or("0".into())
        );
        platform.insert("desktop-startup-id", Value::from(startup.as_str()));
        zbus::blocking::Connection::session()?.call_method(
            Some(id),
            instance::object_path(id).as_str(),
            Some("org.freedesktop.Application"),
            "Open",
            &(uris(), platform),
        )?;
    }
    #[cfg(not(target_os = "linux"))]
    if !matches!(
        instance::claim(id, Request::Open { uris: uris() })?,
        Claim::Forwarded
    ) {
        return Err("secondary became primary".into());
    }
    let until = Instant::now() + Duration::from_secs(15);
    while !done.exists() && Instant::now() < until {
        app.wait_timeout(50)?;
    }
    if !done.exists() {
        return Err("primary did not acknowledge focus".into());
    }
    Ok(())
}
fn primary(app: &App, id: &str, done: &Path) -> Result<()> {
    let Claim::Primary(listener) = instance::claim(id, Request::Activate)? else {
        return Err("initial ownership failed".into());
    };
    let window = show(app, "Existing primary")?;
    listener.wake_with(app.waker());
    let mut launcher = Running(
        Command::new(std::env::current_exe()?)
            .arg("--child")
            .arg(id)
            .arg(done)
            .spawn()?,
    );
    let until = Instant::now() + Duration::from_secs(20);
    let forwarded = loop {
        if let Some(item) = listener.take_forwarded().into_iter().next() {
            break item;
        }
        if launcher.0.try_wait()?.is_some() {
            return Err("secondary exited before forwarding".into());
        }
        if Instant::now() >= until {
            return Err("forwarding timed out".into());
        }
        app.wait_timeout(100)?;
    };
    if forwarded.request != (Request::Open { uris: uris() }) {
        return Err("forwarding lost Unicode paths or URI schemes".into());
    }
    window.focus_with_context(&forwarded.activation)?;
    let until = Instant::now() + Duration::from_secs(5);
    while !window.has_focus() && Instant::now() < until {
        app.wait_timeout(20)?;
    }
    if !window.has_focus() {
        return Err("existing primary did not receive keyboard focus".into());
    }
    #[cfg(target_os = "linux")]
    {
        let began = cpu_ns()?;
        pump(app, Duration::from_millis(500))?;
        let used = cpu_ns()? - began;
        if used > 50_000_000 {
            return Err(format!("native worker did not return to idle: {used} CPU ns").into());
        }
        println!("native idle: {used} CPU ns in 500 ms");
    }
    std::fs::write(done, "focused")?;
    if !launcher.0.wait()?.success() {
        return Err("secondary failed".into());
    }
    listener.close();
    if !matches!(instance::claim(id, Request::Activate)?, Claim::Primary(_)) {
        return Err("listener ownership survived close".into());
    }
    println!("single-instance: Unicode forwarding, native focus, close and restart: ok");
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let is_child = args.get(1).is_some_and(|arg| arg == "--child");
    let id = if is_child {
        args[2].clone()
    } else {
        format!("dev.gossamer.native_instance.p{}", std::process::id())
    };
    let done = if is_child {
        PathBuf::from(&args[3])
    } else {
        std::env::temp_dir().join(&id)
    };
    let app = gos_windowing::init(
        "Instance verification",
        &InitOptions {
            app_id: id.clone(),
            ..Default::default()
        },
    )?;
    let result = if is_child {
        child(&app, &id, &done)
    } else {
        primary(&app, &id, &done)
    };
    app.finalize();
    if !is_child {
        let _ = std::fs::remove_file(done);
    }
    result
}
