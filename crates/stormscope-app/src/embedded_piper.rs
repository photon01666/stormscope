//! First-run extraction of the Piper runtime/voice embedded in the Windows
//! single-file release.
//!
//! The CI builds with `--features embedded-piper`, which bakes
//! `assets/piper-windows.zip` into the executable. On first launch we write
//! that zip to `%LOCALAPPDATA%\stormscope` and let Windows' own
//! `Expand-Archive` unpack the `piper/` tree there. Subsequent launches see
//! the marker and skip the work. The rest of the voice code already knows how
//! to find `%LOCALAPPDATA%\stormscope\piper`.

#![cfg(windows)]

include!(concat!(env!("OUT_DIR"), "/piper_embed.rs"));

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Extract the embedded bundle if this is the first run (or an interrupted
/// extraction left no marker).
pub fn ensure_extracted() {
    if PIPER_BUNDLE.is_empty() {
        return;
    }
    let Some(root) = local_root() else {
        log::warn!("voice: LOCALAPPDATA unavailable; cannot extract embedded Piper");
        return;
    };
    let piper = root.join("piper");
    let model = piper.join("voices/en/en_US/lessac/high/en_US-lessac-high.onnx");
    let marker = root.join(format!(
        "piper-bundle-{}-{}.ok",
        env!("CARGO_PKG_VERSION"),
        PIPER_BUNDLE.len()
    ));
    if marker.is_file() && piper.join("piper.exe").is_file() && model.is_file() {
        return;
    }

    log::info!(
        "voice: extracting embedded Piper bundle to {}",
        root.display()
    );
    if let Err(e) = std::fs::create_dir_all(&root) {
        log::warn!("voice: cannot create {}: {e}", root.display());
        return;
    }
    let zip = root.join("piper-bundle.zip");
    if let Err(e) = std::fs::write(&zip, PIPER_BUNDLE) {
        log::warn!("voice: cannot write embedded Piper zip: {e}");
        return;
    }
    let extracted = extract(&zip, &root);
    let _ = std::fs::remove_file(&zip);
    if extracted && piper.join("piper.exe").is_file() && model.is_file() {
        let _ = std::fs::write(&marker, b"ok\n");
        log::info!("voice: embedded Piper voice ready");
    } else {
        log::warn!("voice: embedded Piper extraction failed; using the system voice");
    }
}

fn local_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|base| base.join("stormscope"))
}

fn extract(zip: &Path, root: &Path) -> bool {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = "$ErrorActionPreference = 'Stop'; \
                  Expand-Archive -LiteralPath $env:STORMSCOPE_PIPER_ZIP \
                  -DestinationPath $env:STORMSCOPE_PIPER_ROOT -Force";
    for exe in ["powershell.exe", "pwsh.exe"] {
        let mut command = Command::new(exe);
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                script,
            ])
            .env("STORMSCOPE_PIPER_ZIP", zip)
            .env("STORMSCOPE_PIPER_ROOT", root)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if command
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}
