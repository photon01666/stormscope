use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let app = std::env::var("STORMSCOPE_APP_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            manifest.join("../../target/x86_64-pc-windows-msvc/release/stormscope.exe")
        });
    println!("cargo:rerun-if-env-changed=STORMSCOPE_APP_EXE");
    println!("cargo:rerun-if-changed={}", app.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::copy(&app, out.join("stormscope-app.exe")).unwrap_or_else(|error| {
        panic!(
            "cannot embed {} in the installer: {error}. Build the app first or set STORMSCOPE_APP_EXE.",
            app.display()
        )
    });
    std::fs::write(
        out.join("app_embed.rs"),
        "pub static APP_EXE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/stormscope-app.exe\"));\n",
    )
    .expect("write app_embed.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        if let Err(error) = res.compile() {
            println!("cargo:warning=could not embed the installer icon: {error}");
        }
    }
}
