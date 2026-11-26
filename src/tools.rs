//! Finding and running ffmpeg / ffprobe.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

fn exe_name(n: &str) -> String {
    if cfg!(windows) { format!("{n}.exe") } else { n.to_string() }
}

/// $FFMPEG / $FFPROBE, then next to this program (portable installs), then PATH,
/// then the usual Homebrew / Linux spots (apps opened from Finder or a desktop
/// menu don't get the shell's PATH).
fn find_tool(name: &str) -> PathBuf {
    if let Ok(p) = std::env::var(name.to_uppercase()) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let file = exe_name(name);
    let mut dirs: Vec<PathBuf> = vec![];
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(dir.join("ffmpeg"));
        dirs.push(dir);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&paths));
    }
    if cfg!(windows) {
        // winget puts ffmpeg here and only adds it to PATH for *new* sessions
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft").join("WinGet").join("Links"));
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
    dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from(file))
}

pub fn ffmpeg() -> &'static Path {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| find_tool("ffmpeg"))
}

pub fn ffprobe() -> &'static Path {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| find_tool("ffprobe"))
}

/// A Command that never flashes a console window on Windows.
pub fn command(tool: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(tool);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.stdin(Stdio::null());
    c
}

pub fn run_bytes<I, S>(tool: &Path, args: I, cwd: Option<&Path>) -> Result<Vec<u8>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut c = command(tool);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c.output().map_err(|e| format!("Couldn't start {}: {e}", tool.display()))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

pub fn run<I, S>(tool: &Path, args: I, cwd: Option<&Path>) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_bytes(tool, args, cwd).map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub fn os(v: &[&str]) -> Vec<OsString> {
    v.iter().map(OsString::from).collect()
}

/// ffmpeg reads `%d` in an image file name as a numbered sequence; turn that off.
pub fn input_args(path: &Path) -> Vec<OsString> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut a = vec![];
    if name.contains('%') && matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "bmp" | "tif" | "tiff" | "webp") {
        a.extend(os(&["-f", "image2", "-pattern_type", "none"]));
    }
    a.push("-i".into());
    a.push(path.as_os_str().to_owned());
    a
}

pub fn install_hint() -> &'static str {
    if cfg!(windows) {
        "Install it with:  winget install Gyan.FFmpeg   (then restart Shrinker)"
    } else if cfg!(target_os = "macos") {
        "Install it with:  brew install ffmpeg   (see brew.sh), then restart Shrinker"
    } else {
        "Install it with:  sudo apt install ffmpeg   (or dnf / pacman), then restart Shrinker"
    }
}

pub fn check() -> Result<(), String> {
    for t in [ffmpeg(), ffprobe()] {
        if run(t, ["-version"], None).is_err() {
            return Err(format!(
                "Shrinker needs ffmpeg, and it isn't installed. {} You can also put ffmpeg and ffprobe next to the Shrinker program.",
                install_hint()
            ));
        }
    }
    Ok(())
}

/// Try a half-second test encode on this machine's graphics-chip encoders.
pub fn detect_gpu() -> Option<String> {
    let candidates: Vec<(&str, &str, Vec<&str>)> = if cfg!(target_os = "macos") {
        vec![("videotoolbox", "hevc_videotoolbox", vec!["-q:v", "60"])]
    } else {
        vec![("nvenc", "hevc_nvenc", vec![]), ("qsv", "hevc_qsv", vec!["-vf", "format=nv12"])]
    };
    for (kind, enc, extra) in candidates {
        let mut a = os(&["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30", "-t", "0.5"]);
        a.extend(os(&extra));
        a.extend(os(&["-c:v", enc, "-f", "null", "-"]));
        if run(ffmpeg(), &a, None).is_ok() {
            return Some(kind.to_string());
        }
    }
    None
}

pub struct Probe {
    pub duration: f64,
    pub start_time: f64,
    pub bitrate: f64,
    pub width: u32,
    pub height: u32,
    pub codec: Option<String>,
    pub alpha: bool,
}

fn num(v: &serde_json::Value) -> f64 {
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64()).unwrap_or(0.0)
}

pub fn probe(p: &Path) -> Result<Probe, String> {
    let mut a = os(&[
        "-v", "error", "-show_entries",
        "format=duration,bit_rate,start_time:stream=codec_type,codec_name,width,height,pix_fmt",
        "-of", "json",
    ]);
    // probe takes the same input options as ffmpeg, minus "-i"
    a.extend(input_args(p).into_iter().filter(|x| x != "-i"));
    let out = run(ffprobe(), &a, None)?;
    let j: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let streams = j["streams"].as_array().cloned().unwrap_or_default();
    let v = streams.iter().find(|s| s["codec_type"] == "video");
    let pix = v.and_then(|s| s["pix_fmt"].as_str()).unwrap_or("");
    Ok(Probe {
        duration: num(&j["format"]["duration"]),
        start_time: num(&j["format"]["start_time"]),
        bitrate: num(&j["format"]["bit_rate"]),
        width: v.and_then(|s| s["width"].as_u64()).unwrap_or(0) as u32,
        height: v.and_then(|s| s["height"].as_u64()).unwrap_or(0) as u32,
        codec: v.and_then(|s| s["codec_name"].as_str()).map(String::from),
        // pixel formats with an alpha channel; palette PNGs (pal8) may carry transparency too
        alpha: ["rgba", "bgra", "argb", "abgr", "ya", "yuva", "gbrap"].iter().any(|p| pix.starts_with(p)) || pix == "pal8",
    })
}
