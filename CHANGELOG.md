# Changelog

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
