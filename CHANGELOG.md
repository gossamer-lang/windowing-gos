# Changelog

## 0.1.20 - Quit signals reach an idle app on macOS

- macOS: SIGTERM and SIGINT end an idle app's wait, so `kill`, `pkill` and logout quit it at once. SDL only marked the quit for the next event, and an idle app on macOS could wait for one indefinitely.

## 0.1.19 - Waits end on time

- `App::wait_timeout` no longer returns before its timeout has passed on the clock its caller keeps, so a timer is not woken for early. SDL times its waits on a clock the system's time correction does not slow, and the two drift apart while the system clock is corrected.

## 0.1.18 - Transparent windows on X11

- X11: a transparent window gets a 32-bit visual, so a compositor blends it with what lies behind. SDL gave such windows a 24-bit visual, which has no alpha.
- `VERSION` names the library's own version again.

## 0.1.17 - Overlays above the window's surface

- macOS: `Window::paint_surface_rings` also takes `overlays`, small areas such as a blinking cursor's cell, each shown in a layer of its own above the surface and the rings. Showing, hiding or moving one repaints nothing. Its pixels are the finished ones the caller painted, so it looks exactly as it would painted into the surface.
- macOS: a window frees its spare buffers 250 ms after its last paint, without waiting for the system to finish showing them.

## 0.1.16 - Scrolling without copying on macOS

- macOS: `Window::paint_surface_rings` shows a ring of rows, such as a terminal's scrolling text, in layers of its own below the window's surface. Scrolling changes which rows the layers show instead of copying pixels, and each frame copies only the rows that changed. `Window::shows_rings` says whether a window can do this; elsewhere `paint_surface_rings` works like `paint_surface`.
- macOS: a frame paints into the most recent buffer the system is not showing, and a window remembers what its last 8 frames changed, so a buffer the system held for a few frames is brought up to date by copying only those areas instead of the whole window.
- macOS: whether VoiceOver is running is checked at most once a second instead of on every frame.

## 0.1.15 - Fewer copies per frame, buffers back sooner

- A frame copies from the frame before only what it does not repaint itself, and copies each pixel once: a terminal that scrolls repaints its whole screen, which macOS and Wayland windows used to copy in from the last frame or two first.
- macOS: a window that stops painting keeps only the buffer it shows much sooner, so a blinking caret no longer holds the spare buffers between blinks.
## 0.1.14 - Windows macOS does not save

- macOS: windows are not restorable, so AppKit no longer encodes and writes a window's state for the next launch, which SDL tells it not to restore, each time the window moves, resizes or changes.

## 0.1.13 - Leaner macOS windows

- macOS: a window that has painted nothing for 2 seconds frees its spare display buffers instead of only marking them reclaimable, and the next paint makes one again.
- macOS: programs no longer load CloudKit, CoreData, CoreImage, CoreLocation and similar system frameworks the library does not use.
- A program that does not use `redrawing` no longer pays for its event watch on every event: the watch is installed only while a `redrawing` scope runs.

## 0.1.12 - Wakes that reach a waiting loop

- Builds SDL 3.4.20 with a fix for a wake from another thread that could be lost while the loop was about to wait: an event pushed just before the wait began sent no wakeup, so the loop slept until its timeout, or until the next input when it had none.

## 0.1.11 - Wayland windows that open reliably

- Builds SDL 3.4.20 with a fix for a window under libdecor (GNOME and other desktops that draw no title bars) that could wait forever for its first configure while another thread read the display; with nothing else drawing, opening a second window could hang the program.
- On a driver that cannot change a window's border, resizability or always-on-top, as SDL 3.4.20's offscreen driver cannot, setting them answers that the operation is not supported instead of succeeding without effect.

## 0.1.10 - FreeBSD desktops, leaner Wayland windows

- FreeBSD desktops take the X11, Wayland, and D-Bus paths Linux does: drag and drop, global shortcuts, desktop notifications, appearance preferences, focus by activation token, and the single-instance claim on the session bus work there, where each answered unavailable.
- A window under the Wayland driver presents its software surface through shared-memory buffers the compositor reads in place, as X11 does, instead of through an SDL renderer: a program that paints in software no longer loads the GPU driver, Vulkan and LLVM, which took over 100 MB.

## 0.1.9 - Leaner macOS windows

- A window on macOS keeps only the buffer it shows in memory: the buffers it painted earlier frames into go back to the system as soon as the window server stops reading them, so they no longer count toward the program's memory, and the next frame takes them back.

## 0.1.8 - Forwarded command lines

- `instance::Request::CommandLine { directory, arguments }` hands a later launch's own command line, and the folder it ran in, to the running instance: on Linux as the `command-line` action of `org.freedesktop.Application`, elsewhere over the existing loopback forwarding.

## 0.1.7 - Focus from a global shortcut

- On X11 a window that `Window::focus` brings forward after a press of a global shortcut takes focus even when the window manager would hold it back as focus stealing: the request says it is the user's own action. Later requests without a new press stay ordinary.

## 0.1.6 - Every shortcut press, every desktop

- On X11 every press of a global shortcut arrives, including the one after a press that hid or showed a window and the one after a chord whose modifier was let go first.
- `Window::set_on_all_desktops` shows a window on every virtual desktop: sticky on X11, kept when the window is hidden and shown again, and joining every Space on macOS. Windows and Wayland give applications no way to ask.

## 0.1.5 - Platform check under sway

- The `platform-check` example resizes its window before it shows a frame, so it passes under sway, which sizes a floating Xwayland window to each buffer of a new size.

## 0.1.4 - Global shortcuts

- Requires Gossamer 0.76.0 or later.
- `App::register_hotkey` asks the desktop to deliver a key chord as `Event::Hotkey` while any application has focus: a key grab on X11, `RegisterHotKey` on Windows, a Carbon hot key on macOS, and the GlobalShortcuts portal on Wayland, where a later refusal arrives as `Event::HotkeyFailed`. `App::unregister_hotkey` gives it back. A chord another application holds is refused.
- On X11 a hidden window shown again keeps staying on top, maximized or fullscreen, and `Window::focus` asks for focus as of the application's latest input on any of its windows or through a hotkey, so a window shown by a hotkey, or one a drag ends over, takes focus. On Wayland it spends the portal's activation token from the press.
- The X11 event hook and the Windows message hook each serve every part of the library that reads them, so dragging, the desktop's preferences and global shortcuts work together.

## 0.1.3 - Gossamer 0.75.17

- Requires Gossamer 0.75.17 or later.

## 0.1.2 - Gossamer 0.75.16

- Requires Gossamer 0.75.16 or later.

## 0.1.1 - Desktop fonts and bell

- `system::preferences` reports the desktop's interface font size.
- `system::monospace_font` names the desktop's fixed-width font on Linux, with its weight.
- `system::beep` plays the desktop's alert sound.

## 0.1.0

- Initial version.
