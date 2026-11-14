use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=binaries");

    let target_triple = std::env::var("TARGET").expect("Cargo sets TARGET for build scripts");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS")
        .expect("Cargo sets CARGO_CFG_TARGET_OS for build scripts");
    let ext = if target_os == "windows" { ".exe" } else { "" };
    let sidecar = format!("binaries/text-mod-{target_triple}{ext}");

    if !Path::new(&sidecar).is_file() && std::env::var_os("TAURI_CONFIG").is_none() {
        // SAFETY: build.rs executes single-threaded here before tauri_build::build() runs,
        // so mutating the process environment does not race with other threads.
        unsafe {
            std::env::set_var("TAURI_CONFIG", r#"{"bundle":{"externalBin":null}}"#);
        }
    }

    tauri_build::build()
}
