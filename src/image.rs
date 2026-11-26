//! Shrinking images with ffmpeg.
//!
//! Photos become JPEG; anything with transparency (logos, screenshots with
//! alpha) becomes WebP so the transparency survives; "WebP" makes everything
//! WebP (smallest). The phone's rotation tag is applied to the pixels, and all
//! metadata (camera, GPS location) is removed. If the result would be bigger
//! than the original, the original is copied instead.

use crate::media::{self, update, Settings, Shared};
use crate::tools::{self, os};
use std::fs;
use std::path::{Path, PathBuf};

fn out_exts(name: &str) -> Vec<String> {
    let orig = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut v = vec!["jpg".to_string(), "webp".to_string()];
    if !v.contains(&orig) {
        v.push(orig);
    }
    v
}

/// An earlier result for this image, whatever format it came out in.
pub fn existing_output(dir: &Path, names: &[String], name: &str) -> Option<PathBuf> {
    let stem = media::out_stem(names, name);
    out_exts(name).into_iter().map(|e| media::out_dir(dir).join(format!("{stem}.{e}"))).find(|p| p.is_file())
}

fn quality(s: &Settings, webp: bool) -> &'static str {
    match (webp, s.img_quality.as_str()) {
        // libwebp: 0-100, higher is better
        (true, "small") => "55",
        (true, "high") => "88",
        (true, _) => "75",
        // mjpeg -q:v: 2-31, lower is better
        (false, "small") => "8",
        (false, "high") => "2",
        (false, _) => "4",
    }
}

pub fn process(sh: &Shared, name: &str) -> Result<(), String> {
    let snap = media::snapshot(sh, name)?;
    let f = &snap.file;
    let src = PathBuf::from(&f.path);
    if !src.exists() {
        return Err("This file was moved or deleted after the folder was opened.".into());
    }
    update(sh, name, |f| {
        f.status = "encoding".into();
        f.progress = 0.05;
        f.note = None;
    });
    let s = &snap.settings;
    let webp = s.img_format == "webp" || f.alpha;
    let ext = if webp { "webp" } else { "jpg" };
    let out_dir = media::out_dir(&snap.dir);
    fs::create_dir_all(&out_dir).map_err(|e| format!("Couldn't create {}: {e}", out_dir.display()))?;
    let stem = media::out_stem(&snap.names, name);
    let out = out_dir.join(format!("{stem}.{ext}"));
    // ffmpeg writes to a plain temporary name (cwd = output folder) so `%` or
    // quotes in the real names can't confuse it, then we rename.
    // (one per image, since several images are shrunk side by side)
    let hash = name.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    let tmp_name = format!(".shrinking-{hash:08x}.{ext}");
    let tmp = out_dir.join(&tmp_name);

    let mut a = os(&["-hide_banner", "-v", "error", "-y"]);
    a.extend(tools::input_args(&src));
    let mut vf = vec![];
    if s.img_size != "keep" {
        let n = &s.img_size; // caps the long side
        vf.push(format!("scale='if(gte(iw,ih),min(iw,{n}),-1)':'if(gte(iw,ih),-1,min(ih,{n}))':flags=lanczos"));
    }
    if !webp {
        vf.push("format=yuvj420p".into());
    }
    if !vf.is_empty() {
        a.extend(os(&["-vf", &vf.join(",")]));
    }
    a.extend(os(&["-frames:v", "1", "-map_metadata", "-1"]));
    if webp {
        a.extend(os(&["-c:v", "libwebp", "-quality", quality(s, true), "-compression_level", "6", "-f", "webp"]));
    } else {
        a.extend(os(&["-c:v", "mjpeg", "-q:v", quality(s, false), "-f", "image2", "-update", "1"]));
    }
    a.push(tmp_name.clone().into());
    let r = tools::run(tools::ffmpeg(), &a, Some(&out_dir));
    if let Err(e) = r {
        let _ = fs::remove_file(&tmp);
        return Err(crate::video::explain(&e, None));
    }

    // Already well compressed? Keep the original instead of a bigger copy.
    let new_size = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(u64::MAX);
    let (final_path, note) = if new_size >= f.size {
        let _ = fs::remove_file(&tmp);
        let orig_ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let keep = out_dir.join(format!("{stem}.{orig_ext}"));
        fs::copy(&src, &keep).map_err(|e| e.to_string())?;
        (keep, Some("Already small; kept the original".to_string()))
    } else {
        // remove results of this image in other formats from earlier runs
        for e in out_exts(name) {
            let p = out_dir.join(format!("{stem}.{e}"));
            if p != out {
                let _ = fs::remove_file(p);
            }
        }
        let _ = fs::remove_file(&out);
        fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
        (out, if f.alpha && s.img_format != "webp" { Some("Saved as WebP to keep transparency".to_string()) } else { None })
    };
    let size = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    update(sh, name, |f| {
        f.status = "done".into();
        f.progress = 1.0;
        f.out_size = size;
        f.output = Some(final_path.display().to_string());
        f.note = note;
    });
    Ok(())
}
