//! Shrinking videos, with stop/resume.
//!
//! Each video is encoded in ~10-second MPEG-TS chunks kept in
//! `<folder>/compressed/.parts/<video>/`. Stopping (or a crash, or closing the
//! window) loses at most the chunk in progress. Resuming counts the frames
//! already encoded and seeks the source to exactly the next frame, and every
//! run writes one continuous timeline, so the chunks join byte-for-byte into
//! the final .mp4 without re-encoding.

use crate::media::{self, lock, update, Settings, Shared};
use crate::tools::{self, os};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Seconds per chunk.
const SEG: u32 = 10;
/// Every run's timestamps start this far in. ffmpeg nudges a run's clock
/// forward by the encoder's delay only when it starts at zero, which would make
/// a resumed run land a couple of frames early; starting all runs past zero
/// treats them alike. The offset is removed when the chunks are joined.
const TS_BASE: f64 = 10.0;

/// Saved next to the chunks as state.json.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Job {
    settings: Settings,
    resume_at: f64,
    parts: Vec<String>,
    runs: u32,
    frames: u64,
    total_frames: u64,
}

pub fn out_path(dir: &Path, names: &[String], name: &str) -> PathBuf {
    media::out_dir(dir).join(format!("{}.mp4", media::out_stem(names, name)))
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h as u32
}

/// Chunk folders get a plain ASCII name: the video's own name may hold
/// characters ffmpeg treats specially (`%`, quotes) or emoji.
fn work_dir(dir: &Path, name: &str) -> PathBuf {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut slug = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut slug: String = slug.trim_matches('-').chars().take(40).collect();
    if slug.is_empty() {
        slug = "video".into();
    }
    media::out_dir(dir).join(".parts").join(format!("{slug}-{:08x}", fnv1a(name)))
}

fn read_job(wd: &Path) -> Option<Job> {
    serde_json::from_str(&fs::read_to_string(wd.join("state.json")).ok()?).ok()
}

fn write_job(wd: &Path, job: &Job) -> Result<(), String> {
    fs::create_dir_all(wd).map_err(|e| e.to_string())?;
    fs::write(wd.join("state.json"), serde_json::to_string_pretty(job).unwrap_or_default()).map_err(|e| e.to_string())
}

fn parse_csv(file: &Path) -> Vec<(String, f64, f64)> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split(',');
            let n = it.next()?.to_string();
            let s = it.next()?.parse().ok()?;
            let e = it.next()?.parse().ok()?;
            Some((n, s, e))
        })
        .collect()
}

fn parts_size(wd: &Path) -> u64 {
    fs::read_dir(wd)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().ends_with(".ts"))
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// If this video was stopped part-way, mark it "paused" and return true.
pub fn resume_status(dir: &Path, f: &mut media::MediaFile) -> bool {
    let wd = work_dir(dir, &f.name);
    let Some(job) = read_job(&wd) else { return false };
    // chunks of a run that never got settled (the app was closed mid-encode)
    let extra = parse_csv(&wd.join(format!("run{}.csv", job.runs)));
    if job.resume_at <= 0.0 && extra.is_empty() {
        return false;
    }
    let more = extra.last().map(|x| x.2).unwrap_or(0.0);
    f.status = "paused".into();
    f.progress = ((job.resume_at + more) / f.duration).min(1.0);
    f.out_size = parts_size(&wd);
    f.job_settings = Some(job.settings);
    true
}

pub fn discard(dir: &Path, name: &str) {
    let _ = fs::remove_dir_all(work_dir(dir, name));
}

fn crf(s: &Settings) -> u32 {
    let base = match s.quality.as_str() {
        "small" => 30,
        "high" => 22,
        _ => 26,
    };
    if s.codec == "h264" { base - 3 } else { base }
}

fn video_args(s: &Settings, gpu: Option<&str>) -> Vec<String> {
    let hw = if s.encoder == "gpu" { gpu } else { None };
    // Encoders need an even width/height; phone and screen recordings are sometimes odd.
    let mut vf = if s.resolution == "keep" {
        "scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string()
    } else {
        let n = &s.resolution; // caps the short side, so portrait videos work too
        format!("scale=w='if(gte(iw,ih),-2,trunc(min(iw,{n})/2)*2)':h='if(gte(iw,ih),trunc(min(ih,{n})/2)*2,-2)'")
    };
    vf.push_str(if hw == Some("qsv") { ",format=nv12" } else { ",format=yuv420p" });
    let codec = if s.codec == "h264" { "h264" } else { "hevc" };
    let q = crf(s).to_string();
    let mut a: Vec<String> = vec!["-vf".into(), vf];
    let mut add = |v: &[&str]| a.extend(v.iter().map(|x| x.to_string()));
    match hw {
        Some("qsv") => add(&["-c:v", &format!("{codec}_qsv"), "-global_quality", &q, "-preset", "medium", "-forced_idr", "1"]),
        Some("nvenc") => add(&["-c:v", &format!("{codec}_nvenc"), "-rc", "vbr", "-cq", &q, "-b:v", "0", "-preset", "p5", "-forced-idr", "1"]),
        Some("videotoolbox") => {
            // VideoToolbox quality runs 1-100, higher is better.
            let vq = match s.quality.as_str() { "small" => "50", "high" => "75", _ => "62" };
            add(&["-c:v", &format!("{codec}_videotoolbox"), "-q:v", vq, "-allow_sw", "1"])
        }
        _ if codec == "hevc" => add(&["-c:v", "libx265", "-crf", &q, "-preset", "fast", "-x265-params", "log-level=error:open-gop=0", "-forced-idr", "1"]),
        _ => add(&["-c:v", "libx264", "-crf", &q, "-preset", "fast", "-forced-idr", "1"]),
    }
    add(&["-force_key_frames", &format!("expr:gte(t,n_forced*{SEG})")]);
    a
}

static CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
fn child_slot() -> MutexGuard<'static, Option<Child>> {
    CHILD.get_or_init(|| Mutex::new(None)).lock().unwrap_or_else(|e| e.into_inner())
}

/// Stop the running encode (its finished chunks are kept).
pub fn kill() {
    if let Some(c) = child_slot().as_mut() {
        let _ = c.kill();
    }
}

struct Src {
    name: String,
    path: PathBuf,
    duration: f64,
    start_time: f64,
}

/// Timestamps of every video frame in the source, in order (no decoding, so fast).
fn source_frame_pts(src: &Src) -> Result<Vec<f64>, String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<f64>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&src.path) {
        return Ok(v.clone());
    }
    let mut a = os(&["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time", "-of", "csv=p=0"]);
    a.push(src.path.as_os_str().to_owned());
    let out = tools::run(tools::ffprobe(), &a, None)?;
    // Frames timed before the start are hidden pre-roll left by trimming; players skip them.
    let mut pts: Vec<f64> = out
        .lines()
        .filter_map(|l| l.trim().parse::<f64>().ok())
        .filter(|t| *t >= src.start_time - 1e-6)
        .collect();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(src.path.clone(), pts.clone());
    Ok(pts)
}

fn count_frames(file: &Path) -> Result<u64, String> {
    let mut a = os(&["-v", "error", "-count_packets", "-select_streams", "v:0", "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"]);
    a.push(file.as_os_str().to_owned());
    Ok(tools::run(tools::ffprobe(), &a, None)?.trim().parse().unwrap_or(0))
}

/// After a run ends (finished, stopped or crashed): keep finished chunks,
/// delete the half-written one, and work out where to resume.
///
/// The resume point comes from counting frames, not the chunk list's times:
/// encoders shift their output clock by a frame or two. With
/// `-fps_mode passthrough`, output frame N is source frame N.
fn settle_run(src: &Src, wd: &Path, job: &mut Job, run_idx: u32) -> Result<usize, String> {
    let list = parse_csv(&wd.join(format!("run{run_idx}.csv")));
    let prefix = format!("r{run_idx}_");
    for e in fs::read_dir(wd).map_err(|e| e.to_string())?.filter_map(Result::ok) {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with(&prefix) && !list.iter().any(|x| x.0 == n) {
            let _ = fs::remove_file(e.path());
        }
    }
    for (name, _, _) in &list {
        job.parts.push(name.clone());
        job.frames += count_frames(&wd.join(name))?;
    }
    job.runs = run_idx + 1;
    if !list.is_empty() {
        let pts = source_frame_pts(src)?;
        job.total_frames = pts.len() as u64;
        job.resume_at = match pts.get(job.frames as usize) {
            Some(t) => (t - src.start_time - 0.0005).max(0.0),
            None => src.duration,
        };
    }
    write_job(wd, job)?;
    Ok(list.len())
}

fn encode_run(sh: &Shared, src: &Src, wd: &Path, job: &Job, run_idx: u32, gpu: Option<&str>) -> Result<(Option<i32>, String), String> {
    let mut a = os(&["-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1", "-y"]);
    if job.resume_at > 0.0 {
        a.extend(os(&["-ss", &format!("{:.6}", job.resume_at)]));
    }
    a.push("-i".into());
    a.push(src.path.as_os_str().to_owned());
    a.extend(os(&["-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn", "-fps_mode", "passthrough"]));
    a.extend(video_args(&job.settings, gpu).into_iter().map(OsString::from));
    a.extend(os(&["-c:a", "aac", "-b:a", "160k"]));
    // One continuous timeline across chunks and runs, so joining is a plain byte join.
    a.extend(os(&["-output_ts_offset", &format!("{:.6}", TS_BASE + job.resume_at)]));
    // Relative names (cwd = chunk folder): a `%` anywhere in the user's path can't
    // be mistaken for the chunk-number pattern.
    a.extend(os(&[
        "-f", "segment", "-segment_time", &SEG.to_string(), "-reset_timestamps", "0",
        "-segment_list", &format!("run{run_idx}.csv"), "-segment_list_type", "csv",
        &format!("r{run_idx}_%05d.ts"),
    ]));

    let mut c = tools::command(tools::ffmpeg());
    c.args(&a).current_dir(wd).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("Couldn't start ffmpeg: {e}"))?;
    let stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    *child_slot() = Some(child);

    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let mut last_size = Instant::now() - Duration::from_secs(5);
    let mut out_time: Option<f64> = None;
    let mut speed: Option<f64> = None;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k {
            "out_time_us" => out_time = v.parse::<f64>().ok().map(|us| us / 1e6),
            "speed" => speed = v.trim_end_matches('x').parse::<f64>().ok().filter(|s| *s > 0.0),
            "progress" => {
                let size = if last_size.elapsed() > Duration::from_secs(1) {
                    last_size = Instant::now();
                    Some(parts_size(wd))
                } else {
                    None
                };
                let (dur, base) = (src.duration, job.resume_at);
                update(sh, &src.name, |f| {
                    if let Some(t) = out_time {
                        // ffmpeg's clock can wobble back a little; never show progress going backwards
                        f.progress = f.progress.max(((base + t) / dur).clamp(0.0, 0.999));
                    }
                    if let Some(sp) = speed {
                        f.speed = Some(sp);
                        f.eta = Some(dur * (1.0 - f.progress) / sp);
                    }
                    if let Some(s) = size {
                        f.out_size = s;
                    }
                });
            }
            _ => {}
        }
    }
    let child = child_slot().take();
    let code = child.and_then(|mut c| c.wait().ok()).and_then(|s| s.code());
    let err = err_thread.join().unwrap_or_default();
    Ok((code, err))
}

pub fn explain(err: &str, code: Option<i32>) -> String {
    let t = err.to_lowercase();
    if t.contains("no space left") || t.contains("disk full") {
        return "The disk is full. Free some space, then press Start to carry on.".into();
    }
    if t.contains("permission denied") {
        return "Access to the file or folder was refused (is it open in another app?).".into();
    }
    if t.contains("invalid data") || t.contains("error while decoding") {
        return "Part of this file is damaged and can't be read.".into();
    }
    if t.contains("no such file") {
        return "The file or its folder isn't there any more (drive unplugged?).".into();
    }
    match err.trim().lines().last() {
        Some(l) if !l.is_empty() => l.chars().take(300).collect(),
        _ => format!("ffmpeg stopped (code {})", code.map_or("?".into(), |c| c.to_string())),
    }
}

/// Join the chunks into the final .mp4 without re-encoding.
fn finalize(wd: &Path, job: &Job, out: &Path) -> Result<(), String> {
    let tmp = wd.join("joined.mp4");
    let mut a = os(&["-hide_banner", "-v", "error", "-y", "-i", &format!("concat:{}", job.parts.join("|"))]);
    a.extend(os(&["-map", "0", "-c", "copy", "-movflags", "+faststart"]));
    if job.settings.codec == "hevc" {
        a.extend(os(&["-tag:v", "hvc1"])); // so iPhones and Macs play it
    }
    a.push("joined.mp4".into());
    tools::run(tools::ffmpeg(), &a, Some(wd)).map_err(|e| format!("Joining failed: {}", explain(&e, None)))?;
    let _ = fs::remove_file(out); // an older result of the same video
    fs::rename(&tmp, out).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(wd);
    Ok(())
}

pub fn process(sh: &Shared, name: &str) -> Result<(), String> {
    let snap = media::snapshot(sh, name)?;
    let f = &snap.file;
    let src = Src { name: f.name.clone(), path: PathBuf::from(&f.path), duration: f.duration, start_time: f.start_time };
    if !src.path.exists() {
        return Err("This file was moved or deleted after the folder was opened.".into());
    }
    let wd = work_dir(&snap.dir, name);
    let mut job = read_job(&wd).unwrap_or(Job { settings: snap.settings.clone(), ..Default::default() });
    fs::create_dir_all(&wd).map_err(|e| format!("Couldn't create {}: {e}", wd.display()))?;
    // The app was closed (or the computer turned off) during the last run: keep its finished chunks.
    let last_run = job.runs;
    if wd.join(format!("run{last_run}.csv")).exists() {
        settle_run(&src, &wd, &mut job, last_run)?;
    }
    write_job(&wd, &job)?;

    lock(sh).current = Some(name.to_string());
    let settings = job.settings.clone();
    update(sh, name, |f| {
        f.status = "encoding".into();
        f.error = None;
        f.job_settings = Some(settings);
    });

    loop {
        let run_idx = job.runs;
        let (code, err) = encode_run(sh, &src, &wd, &job, run_idx, snap.gpu.as_deref())?;
        let got = settle_run(&src, &wd, &mut job, run_idx)?;
        if lock(sh).stopping {
            let mut app = lock(sh);
            if let Some(f) = app.files.iter_mut().find(|f| f.name == name) {
                media::refresh_status(&snap.dir, &snap.names, f);
            }
            return Ok(());
        }
        if code != Some(0) {
            return Err(explain(&err, code));
        }
        // A clean exit means ffmpeg reached the end of the source.
        if code == Some(0) || got == 0 || job.frames >= job.total_frames {
            break;
        }
    }

    update(sh, name, |f| f.status = "joining".into());
    let out = out_path(&snap.dir, &snap.names, name);
    finalize(&wd, &job, &out)?;
    let size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    update(sh, name, |f| {
        f.status = "done".into();
        f.progress = 1.0;
        f.out_size = size;
        f.output = Some(out.display().to_string());
        f.speed = None;
        f.eta = None;
    });
    Ok(())
}
