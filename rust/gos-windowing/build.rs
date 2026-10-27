use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        link_clang_runtime();
    }
}

// SDL's Objective-C checks `@available` at run time through
// `__isPlatformVersionAtLeast`, which lives in clang's runtime library. Clang
// links that library when it drives a link, but rustc links with
// `-nodefaultlibs`, and std's own copy is internal to Rust, so the runtime
// clang compiled SDL against is linked here.
fn link_clang_runtime() {
    let compiler = env::var("CC").unwrap_or_else(|_| "cc".into());
    let resource_dir = Command::new(&compiler)
        .arg("-print-resource-dir")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()));
    let runtime_dir = resource_dir.map(|dir| dir.join("lib").join("darwin"));
    match runtime_dir {
        Some(dir) if dir.join("libclang_rt.osx.a").is_file() => {
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        _ => println!(
            "cargo:warning={compiler} has no libclang_rt.osx.a; linking SDL needs clang's runtime"
        ),
    }
}
