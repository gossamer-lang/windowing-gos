//! Driver-specific capability reporting. Availability describes the adapter,
//! not permission, attached hardware, compositor policy or runtime certification.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Support {
    Available,
    Conditional,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub support: Support,
    pub detail: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub driver: String,
    pub features: Vec<Capability>,
}
impl Capabilities {
    pub fn get(&self, name: &str) -> Option<&Capability> {
        self.features.iter().find(|feature| feature.name == name)
    }
}
/// Query after init, on the UI thread. Unknown drivers fail closed. Conditional
/// capabilities must be attempted and their Result handled by the application.
pub fn capabilities() -> Capabilities {
    crate::support::require_main_thread("windowing::capabilities");
    let driver = crate::video_driver();
    let mut report = for_driver(&driver);
    if driver == "wayland" {
        for feature in &mut report.features {
            if feature.name == "outbound-drag" && !crate::drag::has_outbound_drag() {
                feature.support = Support::Unavailable;
                feature.detail = "No current pointer seat/data-device source".into();
            }
            #[cfg(target_os = "linux")]
            if feature.name == "activation" && !crate::drag::has_activation() {
                feature.support = Support::Unavailable;
                feature.detail = "Compositor did not advertise xdg_activation_v1".into();
            }
        }
    }
    report
}
fn for_driver(driver: &str) -> Capabilities {
    use Support::*;
    let native = matches!(driver, "x11" | "wayland" | "windows" | "cocoa");
    let local = native || driver == "offscreen";
    let wayland = driver == "wayland";
    let windows = driver == "windows";
    let cocoa = driver == "cocoa";
    let desktop = if native { Conditional } else { Unavailable };
    let mut features = Vec::new();
    let mut add = |name: &str, support, detail: &str| {
        features.push(Capability {
            name: name.into(),
            support,
            detail: detail.into(),
        })
    };
    for name in [
        "window-lifecycle",
        "software-framebuffer",
        "keyboard",
        "text-input",
        "display-enumeration",
        "redraw-wake",
    ] {
        add(
            name,
            if local { Available } else { Unavailable },
            "SDL driver implementation; inspect operation errors",
        );
    }
    for name in [
        "window-parenting",
        "modal-windows",
        "transparency",
        "live-resize",
        "hit-testing",
        "mixed-dpi",
        "pointer-capture",
        "relative-pointer",
        "pointer-confinement",
        "touch",
        "pen",
        "custom-cursors",
        "clipboard-text",
        "clipboard-mime",
        "file-dialogs",
        "message-boxes",
        "inbound-drop",
        "outbound-drag",
        "desktop-preferences",
        "activation",
        "screensaver-inhibition",
    ] {
        add(
            name,
            desktop,
            "Requires native driver, OS/compositor support, permission or hardware; operation may fail",
        );
    }
    add(
        "absolute-placement",
        if native && !wayland {
            Conditional
        } else {
            Unavailable
        },
        "Wayland toplevel placement belongs to the compositor",
    );
    add(
        "always-on-top",
        if native && !wayland {
            Conditional
        } else {
            Unavailable
        },
        "No portable Wayland toplevel stacking request",
    );
    add(
        "window-opacity",
        if native && !wayland {
            Conditional
        } else {
            Unavailable
        },
        "Wayland offers per-pixel transparency instead of global window opacity",
    );
    add(
        "frame-theme",
        if native && !wayland {
            Conditional
        } else {
            Unavailable
        },
        "Wayland frame appearance belongs to compositor/libdecor",
    );
    add(
        "backdrop",
        if windows { Conditional } else { Unavailable },
        "Requires Windows 11 22H2 or newer DWM support",
    );
    add(
        "native-tabbing",
        if cocoa { Conditional } else { Unavailable },
        "AppKit native window tabs",
    );
    add(
        "primary-selection",
        if matches!(driver, "x11" | "wayland") {
            Conditional
        } else {
            Unavailable
        },
        "X11 selection or compositor primary-selection protocol",
    );
    add(
        "surrounding-text",
        Unavailable,
        "SDL 3.4 exposes no surrounding-text or reconversion API",
    );
    add(
        "reconversion",
        Unavailable,
        "SDL 3.4 exposes no reconversion API",
    );
    add(
        "native-handles",
        if native { Available } else { Unavailable },
        "Driver-specific handles; valid only during window lifetime",
    );
    add(
        "opengl",
        if local { Conditional } else { Unavailable },
        "Requires compatible driver, library and context options",
    );
    add(
        "vulkan",
        if native { Conditional } else { Unavailable },
        "Requires Vulkan loader and an appropriately created window",
    );
    add(
        "metal",
        if cocoa { Conditional } else { Unavailable },
        "Requires a Metal-created AppKit window",
    );
    add(
        "tray",
        desktop,
        "Linux also requires a StatusNotifier host and libayatana-appindicator",
    );
    add(
        "notifications",
        desktop,
        "Requires desktop notification service; macOS requires bundle identity and authorization",
    );
    add(
        "native-accessibility",
        if windows || cocoa {
            Conditional
        } else {
            Unavailable
        },
        "UI Automation/NSAccessibility; Linux AT-SPI is toolkit-owned",
    );
    Capabilities {
        driver: driver.into(),
        features,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_platform_operations_are_never_advertised_as_available() {
        for driver in [
            "x11",
            "wayland",
            "windows",
            "cocoa",
            "offscreen",
            "unknown",
            "",
        ] {
            let report = for_driver(driver);
            let names: std::collections::HashSet<_> =
                report.features.iter().map(|f| &f.name).collect();
            assert_eq!(names.len(), report.features.len());
            assert!(report.features.len() >= 40);
            for name in ["surrounding-text", "reconversion"] {
                assert_eq!(report.get(name).unwrap().support, Support::Unavailable);
            }
            if driver == "wayland" {
                for name in [
                    "absolute-placement",
                    "always-on-top",
                    "window-opacity",
                    "frame-theme",
                ] {
                    assert_eq!(report.get(name).unwrap().support, Support::Unavailable);
                }
            }
            if driver == "unknown" || driver.is_empty() {
                assert!(
                    report
                        .features
                        .iter()
                        .all(|f| f.support == Support::Unavailable)
                );
            }
        }
    }
}
