#![cfg_attr(windows, windows_subsystem = "windows")]
//! Tiny per-user Windows installer for Stormscope.
//!
//! The release artifact embeds the real app executable. It installs to
//! `%LOCALAPPDATA%\Programs\Stormscope`, creates Desktop/Start Menu shortcuts,
//! registers an uninstall entry, and can update an existing installation
//! silently (`--silent --update`) for the in-app updater.

#[cfg(not(windows))]
fn main() {
    eprintln!("stormscope-setup is a Windows-only installer");
}

#[cfg(windows)]
mod app_embed {
    include!(concat!(env!("OUT_DIR"), "/app_embed.rs"));
}
#[cfg(windows)]
use app_embed::APP_EXE;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;

#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDCANCEL, IDOK, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_OKCANCEL, MessageBoxW,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const APP_NAME: &str = "Stormscope";
#[cfg(windows)]
const EXE_NAME: &str = "Stormscope.exe";
#[cfg(windows)]
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let silent = args.iter().any(|arg| arg == "--silent" || arg == "/S");
    let update = args.iter().any(|arg| arg == "--update");
    let uninstall = args.iter().any(|arg| arg == "--uninstall");

    if uninstall {
        uninstall_app(silent);
        return;
    }

    if !silent {
        let text = format!(
            "Install {APP_NAME} {VERSION} for this user?\n\n\
             It will be installed to:\n{}\n\n\
             Desktop and Start Menu shortcuts will be created.",
            install_dir().display()
        );
        if !confirm("Install Stormscope", &text) {
            return;
        }
    }

    if let Err(error) = install(update) {
        if !silent {
            message(
                "Stormscope installer",
                &format!("Installation failed:\n\n{error}"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        std::process::exit(1);
    }
    if !silent {
        message(
            "Stormscope installed",
            &format!("{APP_NAME} {VERSION} is installed and ready."),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(windows)]
fn install_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Programs")
        .join(APP_NAME)
}

#[cfg(windows)]
fn install(update: bool) -> Result<(), String> {
    // The running app locks its exe. The installer is a different process, so
    // it can terminate the old copy and replace it.
    if update {
        kill_running_app();
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;

    let target = dir.join(EXE_NAME);
    // Retry briefly in case Windows is still releasing the old executable.
    let mut last = None;
    for _ in 0..20 {
        match std::fs::write(&target, APP_EXE) {
            Ok(()) => {
                last = None;
                break;
            }
            Err(error) => {
                last = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
    }
    if let Some(error) = last {
        return Err(format!("writing {}: {error}", target.display()));
    }

    // Keep a copy of this setup executable as the uninstaller.
    if let Ok(current) = std::env::current_exe() {
        let uninstall = dir.join("Uninstall.exe");
        if current != uninstall {
            let _ = std::fs::copy(&current, &uninstall);
        }
    }

    create_shortcuts(&target)?;
    register_uninstall(&dir, &target);
    launch(&target);
    Ok(())
}

#[cfg(windows)]
fn kill_running_app() {
    let _ = Command::new("taskkill")
        .args(["/IM", EXE_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(windows)]
fn launch(path: &Path) {
    let _ = Command::new(path).spawn();
}

#[cfg(windows)]
fn create_shortcuts(target: &Path) -> Result<(), String> {
    let target = target.display().to_string().replace('\'', "''");
    let dir = install_dir().display().to_string().replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut([Environment]::GetFolderPath('Desktop')+'\\{APP_NAME}.lnk');\
         $s.TargetPath='{target}';$s.WorkingDirectory='{dir}';$s.IconLocation='{target},0';$s.Save();\
         $p=[Environment]::GetFolderPath('Programs')+'\\{APP_NAME}.lnk';\
         $s=(New-Object -ComObject WScript.Shell).CreateShortcut($p);\
         $s.TargetPath='{target}';$s.WorkingDirectory='{dir}';$s.IconLocation='{target},0';$s.Save()"
    );
    run_powershell(&script).map_err(|e| format!("creating shortcuts: {e}"))
}

#[cfg(windows)]
fn register_uninstall(dir: &Path, target: &Path) {
    let dir = dir.display().to_string().replace('\'', "''");
    let target = target.display().to_string().replace('\'', "''");
    let uninstall = format!("{dir}\\Uninstall.exe");
    let script = format!(
        "$k='HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{APP_NAME}';\
         New-Item -Path $k -Force | Out-Null;\
         Set-ItemProperty $k DisplayName '{APP_NAME}' -Force;\
         Set-ItemProperty $k DisplayVersion '{VERSION}' -Force;\
         Set-ItemProperty $k Publisher '{APP_NAME}' -Force;\
         Set-ItemProperty $k InstallLocation '{dir}' -Force;\
         Set-ItemProperty $k DisplayIcon '{target},0' -Force;\
         Set-ItemProperty $k UninstallString '\"{uninstall}\" --uninstall' -Force;\
         New-ItemProperty $k NoModify 1 -PropertyType DWord -Force | Out-Null;\
         New-ItemProperty $k NoRepair 1 -PropertyType DWord -Force | Out-Null"
    );
    let _ = run_powershell(&script);
}

#[cfg(windows)]
fn uninstall_app(silent: bool) {
    if !silent {
        if !confirm(
            "Uninstall Stormscope",
            "Remove Stormscope from this user? Your radar cache and settings stay on disk.",
        ) {
            return;
        }
    }
    kill_running_app();
    let dir = install_dir();
    let _ = run_powershell(&format!(
        "Remove-Item ([Environment]::GetFolderPath('Desktop')+'\\{APP_NAME}.lnk') -ErrorAction SilentlyContinue;\
         Remove-Item ([Environment]::GetFolderPath('Programs')+'\\{APP_NAME}.lnk') -ErrorAction SilentlyContinue;\
         Remove-Item 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{APP_NAME}' -Recurse -ErrorAction SilentlyContinue"
    ));
    // The uninstaller is running from the install directory; delete it after
    // this process exits.
    let _ = Command::new("cmd")
        .args([
            "/C",
            "ping",
            "127.0.0.1",
            "-n",
            "3",
            ">",
            "nul",
            "&",
            "rmdir",
            "/s",
            "/q",
        ])
        .arg(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    if !silent {
        message(
            "Stormscope uninstalled",
            "Stormscope has been removed. Your cache and settings were left in place.",
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(windows)]
fn run_powershell(script: &str) -> std::io::Result<()> {
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|_| ())
}

#[cfg(windows)]
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn message(title: &str, text: &str, flags: u32) {
    let title = to_wide(title);
    let text = to_wide(text);
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), flags);
    }
}

#[cfg(windows)]
fn confirm(title: &str, text: &str) -> bool {
    let title = to_wide(title);
    let text = to_wide(text);
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OKCANCEL | MB_ICONQUESTION,
        )
    };
    result == IDOK && result != IDCANCEL
}
