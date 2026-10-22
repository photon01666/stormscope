//! Embed the app icon in the Windows executable.
//!
//! The *window* icon is set at runtime from the embedded PNG (see `main.rs`);
//! this covers the `.exe` itself, which is what Explorer and the taskbar show
//! before/without a running window. On non-Windows hosts this is a no-op.

fn main() {
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
