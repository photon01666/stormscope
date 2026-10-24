//! Embed the app icon and, for the Windows single-file release, the Piper
//! runtime + neural voice bundle.
//!
//! The *window* icon is set at runtime from the embedded PNG (see `main.rs`);
//! this covers the `.exe` itself, which is what Explorer and the taskbar show
//! before/without a running window. On non-Windows hosts this is a no-op.
//!
//! `--features embedded-piper` requires `assets/piper-windows.zip` and bakes
//! it into the executable. The app extracts it to
//! `%LOCALAPPDATA%\stormscope\piper` on first launch. Normal developer builds
//! leave the feature off and still support a sidecar `piper/` folder or the
//! system SAPI voice.

use std::path::PathBuf;

fn embed_piper() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let asset = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("piper-windows.zip");
    println!("cargo:rerun-if-changed={}", asset.display());
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EMBEDDED_PIPER");

    let enabled = std::env::var_os("CARGO_FEATURE_EMBEDDED_PIPER").is_some();
    let generated = if enabled {
        if !asset.is_file() {
            panic!(
                "embedded-piper is enabled but {} is missing; run the Windows \
                 voice bundle step (or download the Piper runtime and lessac \
                 voice) before building",
                asset.display()
            );
        }
        let copied = out_dir.join("piper-windows.zip");
        std::fs::copy(&asset, &copied).expect("copy piper-windows.zip into OUT_DIR");
        "pub static PIPER_BUNDLE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/piper-windows.zip\"));\n".to_string()
    } else {
        "pub static PIPER_BUNDLE: &[u8] = &[];\n".to_string()
    };
    std::fs::write(out_dir.join("piper_embed.rs"), generated)
        .expect("write generated piper_embed.rs");
}

fn main() {
    embed_piper();

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        if let Err(e) = res.compile() {
            // A missing resource compiler (rc.exe) must not break the build:
            // the runtime window icon still works, only the file icon is lost.
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
