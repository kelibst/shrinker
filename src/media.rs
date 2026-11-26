//! What Shrinker knows about a folder: its videos and images, the settings,
//! and the queue that shrinks them (videos one at a time, images in parallel).

use crate::{image, tools, video};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

pub const OUT_DIR: &str = "compressed";
const VIDEO_EXT: &[&str] = &["mp4", "mov", "mkv", "avi", "m4v", "webm", "wmv", "mts", "m2ts", "3gp", "flv"];
const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif"];

// ---------------------------------------------------------------- settings

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    // videos
    pub codec: String,
    pub quality: String,
    pub resolution: String,
    pub encoder: String,
    // images
    pub img_format: String,
    pub img_quality: String,
    pub img_size: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            codec: "hevc".into(),
            quality: "balanced".into(),
            resolution: "keep".into(),
            encoder: "cpu".into(),
            img_format: "auto".into(),
            img_quality: "balanced".into(),
            img_size: "keep".into(),
        }
    }
}

fn number_in(val: &str, lo: u32, hi: u32) -> bool {
    val.parse::<u32>().map(|n| (lo..=hi).contains(&n)).unwrap_or(false)
}

impl Settings {
    pub fn set(&mut self, key: &str, val: &str) -> Result<(), String> {
        let (slot, ok) = match key {
            "codec" => (&mut self.codec, matches!(val, "hevc" | "h264")),
            "quality" => (&mut self.quality, matches!(val, "small" | "balanced" | "high")),
            "resolution" => (&mut self.resolution, val == "keep" || number_in(val, 144, 4320)),
            "encoder" => (&mut self.encoder, matches!(val, "gpu" | "cpu")),
            "imgFormat" => (&mut self.img_format, matches!(val, "auto" | "webp")),
            "imgQuality" => (&mut self.img_quality, matches!(val, "small" | "balanced" | "high")),
            "imgSize" => (&mut self.img_size, val == "keep" || number_in(val, 256, 16384)),
            _ => return Err(format!("Unknown setting: {key}")),
        };
        if !ok {
            return Err(format!("Unknown value for {key}: {val}"));
        }
        *slot = val.to_string();
        Ok(())
    }
}

// ---------------------------------------------------------------- state

#[derive(Clone, Copy, Serialize, PartialEq, Eq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Video,
    Image,
}

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MediaFile {
    pub name: String,
    pub path: String,
    pub kind: Kind,
    /// readable and shrinkable
    pub ok: bool,
    pub size: u64,
    pub duration: f64,
    #[serde(skip)]
    pub start_time: f64,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub alpha: bool,
    pub bitrate: f64,
    pub selected: bool,
    pub status: String,
    pub progress: f64,
    pub out_size: u64,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
    pub error: Option<String>,
    pub note: Option<String>,
    pub output: Option<String>,
    pub job_settings: Option<Settings>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct App {
    pub dir: String,
    #[serde(skip)]
    pub dir_path: PathBuf,
    pub files: Vec<MediaFile>,
    pub running: bool,
    pub stopping: bool,
    pub current: Option<String>,
    pub queue: Vec<String>,
    pub settings: Settings,
    pub gpu: Option<String>,
    pub message: String,
    /// set when ffmpeg is missing: the page shows it as a banner
    pub problem: Option<String>,
    pub version: &'static str,
}

pub type Shared = Arc<Mutex<App>>;

pub fn new_shared() -> Shared {
    Arc::new(Mutex::new(App { version: env!("CARGO_PKG_VERSION"), ..Default::default() }))
}

/// A lock that survives a panicked thread instead of poisoning the whole app.
pub fn lock(sh: &Shared) -> MutexGuard<'_, App> {
    sh.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn update<F: FnOnce(&mut MediaFile)>(sh: &Shared, name: &str, f: F) {
    if let Some(v) = lock(sh).files.iter_mut().find(|v| v.name == name) {
        f(v);
    }
}

/// Everything a worker needs from the shared state, copied out so the lock
/// isn't held while ffmpeg runs.
pub struct Snapshot {
    pub file: MediaFile,
    pub dir: PathBuf,
    pub names: Vec<String>,
    pub settings: Settings,
    pub gpu: Option<String>,
}

pub fn snapshot(sh: &Shared, name: &str) -> Result<Snapshot, String> {
    let app = lock(sh);
    let file = app.files.iter().find(|f| f.name == name).cloned().ok_or("This file is no longer in the list.")?;
    Ok(Snapshot {
        file,
        dir: app.dir_path.clone(),
        names: app.files.iter().map(|f| f.name.clone()).collect(),
        settings: app.settings.clone(),
        gpu: app.gpu.clone(),
    })
}

// ---------------------------------------------------------------- names & paths

fn ext_of(name: &str) -> String {
    Path::new(name).extension().and_then(OsStr::to_str).map(str::to_lowercase).unwrap_or_default()
}

pub fn kind_of(name: &str) -> Option<Kind> {
    let e = ext_of(name);
    if VIDEO_EXT.contains(&e.as_str()) {
        Some(Kind::Video)
    } else if IMAGE_EXT.contains(&e.as_str()) {
        Some(Kind::Image)
    } else {
        None
    }
}

/// The output file's name without extension. "clip.mp4" and "clip.mov" in one
/// folder must not both become clip.mp4, so a shared name gets its extension added.
pub fn out_stem(names: &[String], name: &str) -> String {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let clash = names.iter().any(|n| {
        n != name && Path::new(n).file_stem().map(|s| s.to_string_lossy().to_lowercase()) == Some(stem.to_lowercase())
    });
    if clash { format!("{stem} ({})", ext_of(name)) } else { stem }
}

pub fn out_dir(dir: &Path) -> PathBuf {
    dir.join(OUT_DIR)
}

/// Where the finished file is (or will be): None until there is one.
pub fn output_of(dir: &Path, names: &[String], f: &MediaFile) -> Option<PathBuf> {
    match f.kind {
        Kind::Video => Some(video::out_path(dir, names, &f.name)).filter(|p| p.exists()),
        Kind::Image => image::existing_output(dir, names, &f.name),
    }
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

// ---------------------------------------------------------------- scanning

/// Open a folder, or a file's folder. `pick`: names to tick; None ticks everything.
pub fn scan(sh: &Shared, target: &Path, pick: Option<Vec<String>>) -> Result<(), String> {
    let target = std::path::absolute(target).unwrap_or_else(|_| target.to_path_buf());
    let meta = fs::metadata(&target).map_err(|_| format!("\"{}\" doesn't exist.", target.display()))?;
    let (dir, pick) = if meta.is_file() {
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        (target.parent().map(Path::to_path_buf).unwrap_or_default(), Some(pick.unwrap_or(vec![name])))
    } else {
        (target.clone(), pick)
    };
    let mut names: Vec<String> = fs::read_dir(&dir)
        .map_err(|e| format!("Couldn't open \"{}\": {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| kind_of(n).is_some())
        .collect();
    names.sort_by(|a, b| natural_cmp(a, b));

    {
        let mut app = lock(sh);
        app.dir_path = dir.clone();
        app.dir = dir.display().to_string();
        app.files.clear();
        app.message = if names.is_empty() { "No videos or images in this folder.".into() } else { String::new() };
    }
    for name in &names {
        let full = dir.join(name);
        let kind = kind_of(name).unwrap_or_default();
        let size = fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
        let mut f = MediaFile {
            name: name.clone(),
            path: full.display().to_string(),
            kind,
            size,
            selected: pick.as_ref().map_or(true, |p| p.contains(name)),
            status: "ready".into(),
            ..Default::default()
        };
        let what = if kind == Kind::Video { "video" } else { "image" };
        let checked = if size == 0 {
            Err("This file is empty (0 bytes).".to_string())
        } else {
            match tools::probe(&full) {
                Err(e) if e.contains("Invalid data") || e.contains("moov atom") || e.contains("End of file") || e.contains("Could not find codec") => {
                    Err(format!("This file is damaged or not a real {what} (it may have been cut short while saving)."))
                }
                Err(e) => Err(e.lines().next().unwrap_or("Can't read this file").chars().take(200).collect()),
                Ok(p) if p.codec.is_none() || p.width == 0 => Err(if kind == Kind::Video {
                    "No picture in this file (audio only, or not a video).".to_string()
                } else {
                    "This file is damaged or not a real image.".to_string()
                }),
                Ok(p) if kind == Kind::Video && p.duration <= 0.0 => {
                    Err("Can't tell how long this video is; it may be damaged or still being copied.".into())
                }
                Ok(p) => Ok(p),
            }
        };
        match checked {
            Ok(p) => {
                f.ok = true;
                f.duration = if kind == Kind::Video { p.duration } else { 0.0 };
                f.start_time = p.start_time;
                f.bitrate = p.bitrate;
                f.width = p.width;
                f.height = p.height;
                f.codec = p.codec.unwrap_or_default();
                f.alpha = p.alpha;
                refresh_status(&dir, &names, &mut f);
            }
            Err(e) => {
                f.status = "error".into();
                f.error = Some(e);
                f.selected = false;
            }
        }
        lock(sh).files.push(f);
    }
    Ok(())
}

pub fn refresh_status(dir: &Path, names: &[String], f: &mut MediaFile) {
    if !f.ok {
        return; // unreadable file: keep its error
    }
    f.progress = 0.0;
    f.out_size = 0;
    f.speed = None;
    f.eta = None;
    f.job_settings = None;
    f.output = None;
    f.status = "ready".into();
    if f.kind == Kind::Video && video::resume_status(dir, f) {
        return; // stopped part-way: video sets "paused"
    }
    if let Some(out) = output_of(dir, names, f) {
        f.status = "done".into();
        f.progress = 1.0;
        f.out_size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        f.output = Some(out.display().to_string());
    }
}

// ---------------------------------------------------------------- queue

pub fn start(sh: &Shared) -> Result<(), String> {
    {
        let mut app = lock(sh);
        if app.running {
            return Ok(());
        }
        // images first: they take seconds and show results right away
        let mut queue: Vec<(Kind, String)> = app
            .files
            .iter()
            .filter(|f| f.selected && f.ok && f.status != "done")
            .map(|f| (f.kind, f.name.clone()))
            .collect();
        queue.sort_by_key(|(k, _)| *k == Kind::Video);
        if queue.is_empty() {
            app.message = "Nothing to do. Tick some files that aren't done yet.".into();
            return Err(app.message.clone());
        }
        let names: Vec<String> = queue.into_iter().map(|(_, n)| n).collect();
        for f in app.files.iter_mut().filter(|f| names.contains(&f.name)) {
            if f.status != "paused" {
                f.status = "queued".into();
            }
            f.error = None;
        }
        app.queue = names;
        app.running = true;
        app.stopping = false;
        app.message.clear();
    }
    let sh = sh.clone();
    std::thread::spawn(move || run_queue(&sh));
    Ok(())
}

fn fail(sh: &Shared, name: &str, e: String) {
    update(sh, name, |f| {
        f.status = "error".into();
        f.error = Some(e);
        f.speed = None;
        f.eta = None;
    });
}

fn run_queue(sh: &Shared) {
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4);
    loop {
        // next job: one video, or up to `workers` images side by side
        let batch: Vec<(String, Kind)> = {
            let app = lock(sh);
            if app.stopping {
                break;
            }
            let kind_of_name = |n: &String| app.files.iter().find(|f| &f.name == n).map(|f| f.kind).unwrap_or_default();
            let Some(first) = app.queue.first() else { break };
            if kind_of_name(first) == Kind::Video {
                vec![(first.clone(), Kind::Video)]
            } else {
                app.queue.iter().take_while(|n| kind_of_name(n) == Kind::Image).take(workers).map(|n| (n.clone(), Kind::Image)).collect()
            }
        };
        if batch[0].1 == Kind::Video {
            if let Err(e) = video::process(sh, &batch[0].0) {
                fail(sh, &batch[0].0, e);
            }
        } else {
            std::thread::scope(|s| {
                for (name, _) in &batch {
                    s.spawn(move || {
                        if let Err(e) = image::process(sh, name) {
                            fail(sh, name, e);
                        }
                    });
                }
            });
        }
        let mut app = lock(sh);
        if !app.stopping {
            app.queue.retain(|n| !batch.iter().any(|(b, _)| b == n));
        }
    }
    let mut app = lock(sh);
    let dir = app.dir_path.clone();
    let names: Vec<String> = app.files.iter().map(|f| f.name.clone()).collect();
    for f in app.files.iter_mut().filter(|f| f.status == "queued" || f.status == "encoding") {
        refresh_status(&dir, &names, f);
    }
    app.running = false;
    app.stopping = false;
    app.current = None;
    app.queue.clear();
}

pub fn stop(sh: &Shared) {
    {
        let mut app = lock(sh);
        if !app.running {
            return;
        }
        app.stopping = true;
    }
    video::kill();
}

/// Throw away a stopped video's chunks so it starts over (e.g. with new settings).
pub fn discard(sh: &Shared, name: &str) -> Result<(), String> {
    let mut app = lock(sh);
    if app.running {
        return Err("Stop first.".into());
    }
    let dir = app.dir_path.clone();
    let names: Vec<String> = app.files.iter().map(|f| f.name.clone()).collect();
    video::discard(&dir, name);
    if let Some(f) = app.files.iter_mut().find(|f| f.name == name) {
        f.error = None;
        refresh_status(&dir, &names, f);
    }
    Ok(())
}

pub fn thumbnail(sh: &Shared, name: &str) -> Option<Vec<u8>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    let f = lock(sh).files.iter().find(|f| f.name == name && f.ok).cloned()?;
    let key = format!("{}|{}", f.path, f.size);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(img) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Some(img.clone());
    }
    let mut a = tools::os(&["-v", "error"]);
    if f.kind == Kind::Video {
        a.extend(tools::os(&["-ss", &format!("{:.2}", (f.duration * 0.1).min(30.0))]));
    }
    a.extend(tools::input_args(Path::new(&f.path)));
    // WebP keeps transparent images transparent (JPEG would turn it black)
    a.extend(tools::os(&["-frames:v", "1", "-vf", "scale=240:-2", "-f", "webp", "-c:v", "libwebp", "-quality", "70", "pipe:1"]));
    let img = tools::run_bytes(tools::ffmpeg(), &a, None).ok().filter(|b| !b.is_empty())?;
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, img.clone());
    Some(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["clip10.mp4", "clip2.mp4", "Clip1.mp4", "clip02b.mp4"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Clip1.mp4", "clip2.mp4", "clip02b.mp4", "clip10.mp4"]);
    }

    #[test]
    fn same_name_different_extension_get_separate_outputs() {
        let names: Vec<String> = ["a.mp4", "a.MOV", "b.jpg"].map(String::from).to_vec();
        assert_eq!(out_stem(&names, "a.mp4"), "a (mp4)");
        assert_eq!(out_stem(&names, "a.MOV"), "a (mov)");
        assert_eq!(out_stem(&names, "b.jpg"), "b");
    }

    #[test]
    fn settings_are_validated() {
        let mut s = Settings::default();
        assert!(s.set("quality", "small").is_ok());
        assert_eq!(s.quality, "small");
        assert!(s.set("quality", "huge").is_err());
        assert!(s.set("resolution", "720").is_ok());
        assert!(s.set("resolution", "9").is_err());
        assert!(s.set("imgSize", "1920").is_ok());
        assert!(s.set("imgFormat", "gif").is_err());
        assert!(s.set("nope", "x").is_err());
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of("x.JPG"), Some(Kind::Image));
        assert_eq!(kind_of("x.heic"), Some(Kind::Image));
        assert_eq!(kind_of("x.MOV"), Some(Kind::Video));
        assert_eq!(kind_of("x.txt"), None);
        assert_eq!(kind_of("noext"), None);
    }

    #[test]
    fn saved_settings_from_older_versions_still_load() {
        // a video's state.json from before images existed has no img* keys
        let s: Settings = serde_json::from_str(r#"{"codec":"h264","quality":"high","resolution":"keep","encoder":"gpu"}"#).unwrap();
        assert_eq!(s.codec, "h264");
        assert_eq!(s.img_format, "auto");
    }
}
