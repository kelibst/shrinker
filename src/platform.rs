//! The few things that differ between Windows, macOS and Linux: native
//! folder/file pickers, opening the browser, and showing a file in the file manager.

use crate::tools::command;
use std::path::{Path, PathBuf};

/// Returned when this system has no picker we can drive (e.g. a Linux box
/// without zenity/kdialog); the page then asks for the path as text.
pub const NO_PICKER: &str = "no-picker";

const PROMPT_FOLDER: &str = "Pick a folder of videos or images to shrink";
const PROMPT_FILES: &str = "Pick videos or images to shrink";
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS dialogs filter by type themselves
const EXTS: &[&str] = &["mp4", "mov", "mkv", "avi", "m4v", "webm", "wmv", "mts", "m2ts", "3gp", "flv", "jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif"];

fn lines(out: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(out).lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect()
}

/// Opened by double-click / Start menu on Windows, Shrinker gets a console
/// window of its own; close it (the page has Quit). Run from a terminal, the
/// console is shared and stays.
pub fn hide_own_console() {
    #[cfg(windows)]
    {
        extern "system" {
            fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
            fn FreeConsole() -> i32;
        }
        let mut ids = [0u32; 4];
        // SAFETY: plain Win32 calls with a valid buffer of the stated length.
        unsafe {
            if GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) == 1 {
                FreeConsole();
            }
        }
    }
}

pub fn open_url(url: &str) {
    #[cfg(windows)]
    let r = command("cmd").args(["/c", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = command("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = command("xdg-open").arg(url).spawn();
    if r.is_err() {
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), "  Open {url} in your browser.");
    }
}

/// Show a file selected in the file manager, or open a folder.
pub fn reveal(path: &Path) {
    let is_file = path.is_file();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("explorer.exe");
        if is_file {
            c.raw_arg(format!("/select,\"{}\"", path.display()));
        } else {
            c.arg(path);
        }
        let _ = c.spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = command("open");
        if is_file {
            c.arg("-R");
        }
        let _ = c.arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if is_file { path.parent().unwrap_or(path) } else { path };
        let _ = command("xdg-open").arg(target).spawn();
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
fn powershell(script: &str) -> Result<Vec<PathBuf>, String> {
    let full = format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8\nAdd-Type -AssemblyName System.Windows.Forms\n$o = New-Object System.Windows.Forms.Form -Property @{{TopMost=$true}}\n{script}");
    let out = command("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", &full])
        .output()
        .map_err(|_| NO_PICKER.to_string())?;
    Ok(lines(&out.stdout))
}

#[cfg(windows)]
fn ps_quote(p: &Path) -> String {
    p.display().to_string().replace('\'', "''")
}

#[cfg(windows)]
pub fn pick_folder(start: &Path) -> Result<Option<PathBuf>, String> {
    let r = powershell(&format!(
        "$d = New-Object System.Windows.Forms.FolderBrowserDialog\n$d.Description = '{PROMPT_FOLDER}'\n$d.SelectedPath = '{}'\nif ($d.ShowDialog($o) -eq 'OK') {{ $d.SelectedPath }}",
        ps_quote(start)
    ))?;
    Ok(r.into_iter().next())
}

#[cfg(windows)]
pub fn pick_files(start: &Path) -> Result<Vec<PathBuf>, String> {
    let filter: Vec<String> = EXTS.iter().map(|e| format!("*.{e}")).collect();
    powershell(&format!(
        "$d = New-Object System.Windows.Forms.OpenFileDialog\n$d.Title = '{PROMPT_FILES}'\n$d.Multiselect = $true\n$d.Filter = 'Videos and images|{}|All files|*.*'\n$d.InitialDirectory = '{}'\nif ($d.ShowDialog($o) -eq 'OK') {{ $d.FileNames -join [char]10 }}",
        filter.join(";"),
        ps_quote(start)
    ))
}

// ---------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
fn applescript(script: &str) -> Result<Vec<PathBuf>, String> {
    let out = command("osascript").args(["-e", script]).output().map_err(|_| NO_PICKER.to_string())?;
    // A cancelled dialog exits with an error; that just means "nothing picked".
    Ok(if out.status.success() { lines(&out.stdout) } else { vec![] })
}

#[cfg(target_os = "macos")]
fn as_quote(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
pub fn pick_folder(start: &Path) -> Result<Option<PathBuf>, String> {
    let r = applescript(&format!(
        "POSIX path of (choose folder with prompt \"{PROMPT_FOLDER}\" default location (POSIX file \"{}\"))",
        as_quote(start)
    ))?;
    Ok(r.into_iter().next())
}

#[cfg(target_os = "macos")]
pub fn pick_files(start: &Path) -> Result<Vec<PathBuf>, String> {
    applescript(&format!(
        "set fs to choose file with prompt \"{PROMPT_FILES}\" default location (POSIX file \"{}\") with multiple selections allowed\nset out to \"\"\nrepeat with f in fs\nset out to out & POSIX path of f & linefeed\nend repeat\nreturn out",
        as_quote(start)
    ))
}

// ---------------------------------------------------------------- Linux & other Unix

/// Try zenity, then kdialog. Exit code 1 = the person cancelled.
#[cfg(all(unix, not(target_os = "macos")))]
fn unix_dialog(tries: Vec<(&str, Vec<String>)>) -> Result<Vec<PathBuf>, String> {
    for (tool, args) in tries {
        let Ok(out) = command(tool).args(&args).output() else { continue };
        match out.status.code() {
            Some(0) => return Ok(lines(&out.stdout)),
            Some(1) => return Ok(vec![]),
            _ => continue, // e.g. no display
        }
    }
    Err(NO_PICKER.into())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn pick_folder(start: &Path) -> Result<Option<PathBuf>, String> {
    let s = start.display().to_string();
    let r = unix_dialog(vec![
        ("zenity", vec!["--file-selection".into(), "--directory".into(), format!("--title={PROMPT_FOLDER}"), format!("--filename={s}/")]),
        ("kdialog", vec!["--title".into(), PROMPT_FOLDER.into(), "--getexistingdirectory".into(), s]),
    ])?;
    Ok(r.into_iter().next())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn pick_files(start: &Path) -> Result<Vec<PathBuf>, String> {
    let s = start.display().to_string();
    let globs: Vec<String> = EXTS.iter().flat_map(|e| [format!("*.{e}"), format!("*.{}", e.to_uppercase())]).collect();
    unix_dialog(vec![
        ("zenity", vec![
            "--file-selection".into(), "--multiple".into(), "--separator=\n".into(),
            format!("--title={PROMPT_FILES}"), format!("--filename={s}/"),
            format!("--file-filter=Videos and images | {}", globs.join(" ")),
        ]),
        ("kdialog", vec![
            "--title".into(), PROMPT_FILES.into(), "--getopenfilename".into(), s,
            globs.join(" "), "--multiple".into(), "--separate-output".into(),
        ]),
    ])
}
