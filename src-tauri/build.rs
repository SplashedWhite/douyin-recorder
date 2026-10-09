fn main() {
    tauri_build::build();
    // Tauri normally links its Windows manifest only into application binaries.
    // The headless runtime tests also call Common Controls v6 APIs.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!(
            "cargo:rustc-link-search=native={}",
            std::env::var("OUT_DIR").unwrap()
        );
    }
}
