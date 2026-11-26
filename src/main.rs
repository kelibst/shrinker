//! Shrinker: make videos and images smaller with ffmpeg; stop and resume any time.
//! Runs on Windows, macOS and Linux. See README.md.

mod image;
mod media;
mod platform;
mod server;
mod tools;
mod video;

use media::{lock, Kind, Shared};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "\
Shrinker — make videos and images smaller with ffmpeg. Stop any time; it resumes.

USAGE
  shrinker [FOLDER | FILE...] [OPTIONS]

  Without --cli it opens a page in your browser.
  With --cli it shrinks the given folder or files right here in the terminal.
  Results go to a `compressed` folder next to the originals, which are never changed.

VIDEO OPTIONS
  --quality small|balanced|high       (default balanced)
  --codec hevc|h264                   (default hevc: about half the size of h264)
  --res keep|1080|720|<short side>    (default keep)
  --encoder auto|gpu|cpu              (default auto: the graphics chip when there is one)

IMAGE OPTIONS
  --img-quality small|balanced|high   (default balanced)
  --img-format auto|webp              (default auto: JPEG, or WebP when transparent)
  --img-size keep|2560|1920|<long side>  (default keep)

OTHER
  --cli                     work in the terminal, no browser page
  --only videos|images      in --cli mode, shrink only one kind
  --port N                  first port to try for the page (default 4790)
  --no-browser              don't open the browser automatically
  -V, --version             show the version
  -h, --help                this help

Press Ctrl+C (or Stop on the page) to stop. Run the same command again to carry on.";

struct Args {
    paths: Vec<PathBuf>,
    cli: bool,
    only: Option<Kind>,
    opts: Vec<(&'static str, String)>,
    encoder: String,
    port: u16,
    browser: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { paths: vec![], cli: false, only: None, opts: vec![], encoder: "auto".into(), port: server::DEFAULT_PORT, browser: true };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("shrinker {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--cli" => a.cli = true,
            "--no-browser" => a.browser = false,
            "--quality" => a.opts.push(("quality", val("--quality")?)),
            "--codec" => a.opts.push(("codec", val("--codec")?)),
            "--res" | "--resolution" => a.opts.push(("resolution", val("--res")?)),
            "--img-quality" => a.opts.push(("imgQuality", val("--img-quality")?)),
            "--img-format" => a.opts.push(("imgFormat", val("--img-format")?)),
            "--img-size" => a.opts.push(("imgSize", val("--img-size")?)),
            "--encoder" => a.encoder = val("--encoder")?,
            "--only" => {
                a.only = Some(match val("--only")?.as_str() {
                    "videos" | "video" => Kind::Video,
                    "images" | "image" => Kind::Image,
                    v => return Err(format!("--only must be videos or images, not {v}")),
                })
            }
            "--port" => a.port = val("--port")?.parse().map_err(|_| "--port needs a number")?,
            // macOS adds this when an app is opened from Finder
            s if s.starts_with("-psn_") => {}
            s if s.starts_with('-') => return Err(format!("Unknown option {s}. Try --help.")),
            _ => a.paths.push(PathBuf::from(arg)),
        }
    }
    if !matches!(a.encoder.as_str(), "auto" | "gpu" | "cpu") {
        return Err("--encoder must be auto, gpu or cpu".into());
    }
    Ok(a)
}

fn fatal(msg: &str) -> ! {
    eprintln!("\n  {msg}\n");
    // Double-clicked on Windows: keep the window open long enough to read it.
    if cfg!(windows) && std::env::args().len() <= 1 {
        eprint!("  Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    std::process::exit(1);
}

fn main() {
    let args = parse_args().unwrap_or_else(|e| fatal(&e));
    let sh = media::new_shared();
    {
        let mut app = lock(&sh);
        for (k, v) in &args.opts {
            if let Err(e) = app.settings.set(k, v) {
                drop(app);
                fatal(&e);
            }
        }
    }
    if args.cli {
        if let Err(e) = tools::check() {
            fatal(&e);
        }
        std::process::exit(cli(&sh, &args));
    }

    if args.paths.len() > 1 {
        fatal("The page opens one folder or file at a time. Use --cli to shrink several paths in one go.");
    }
    let start = args.paths.into_iter().next();
    // Already open (e.g. a second double-click, or "Send to")? Hand it over.
    if server::hand_to_running(args.port, start.as_deref(), args.browser) {
        return;
    }
    match tools::check() {
        // Without ffmpeg the page still opens and explains how to install it.
        Err(e) => lock(&sh).problem = Some(e),
        Ok(()) => {
            // The graphics-chip check takes a second or two; the page works meanwhile.
            let sh = sh.clone();
            let encoder = args.encoder.clone();
            std::thread::spawn(move || {
                let gpu = tools::detect_gpu();
                let mut app = lock(&sh);
                app.settings.encoder = if gpu.is_some() && encoder != "cpu" { "gpu".into() } else { "cpu".into() };
                app.gpu = gpu;
            });
        }
    }
    if let Err(e) = server::run(sh, start, args.port, args.browser) {
        fatal(&e);
    }
}

// ---------------------------------------------------------------- terminal mode

fn fmt_size(b: f64) -> String {
    if b >= 1e9 { format!("{:.2} GB", b / 1e9) } else if b >= 1e6 { format!("{:.1} MB", b / 1e6) } else { format!("{:.0} KB", b / 1e3) }
}

fn fmt_time(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    let (h, m, x) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{x:02}") } else { format!("{m}:{x:02}") }
}

/// Group the given paths by folder (files from one folder share a run).
fn cli_targets(paths: &[PathBuf]) -> Vec<(PathBuf, Option<Vec<String>>)> {
    if paths.is_empty() {
        return vec![(std::env::current_dir().unwrap_or_default(), None)];
    }
    let mut out: Vec<(PathBuf, Option<Vec<String>>)> = vec![];
    for p in paths {
        if p.is_file() {
            let dir = std::path::absolute(p).ok().and_then(|a| a.parent().map(|d| d.to_path_buf())).unwrap_or_default();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match out.iter_mut().find(|(d, picks)| *d == dir && picks.is_some()) {
                Some((_, Some(picks))) => picks.push(name),
                _ => out.push((dir, Some(vec![name]))),
            }
        } else {
            out.push((p.clone(), None));
        }
    }
    out
}

fn cli(sh: &Shared, args: &Args) -> i32 {
    let gpu = if args.encoder == "cpu" || args.only == Some(Kind::Image) { None } else { tools::detect_gpu() };
    if args.encoder == "gpu" && gpu.is_none() {
        println!("  No graphics-chip encoder found; videos use the processor.");
    }
    {
        let mut app = lock(sh);
        app.settings.encoder = if gpu.is_some() { "gpu".into() } else { "cpu".into() };
        app.gpu = gpu.clone();
        let s = &app.settings;
        if args.only != Some(Kind::Image) {
            println!(
                "  Videos: {} · {} · {} · {}",
                if s.codec == "hevc" { "H.265" } else { "H.264" },
                s.quality,
                if s.resolution == "keep" { "same size".to_string() } else { format!("{}p", s.resolution) },
                match gpu.as_deref() {
                    Some("qsv") => "Intel Quick Sync",
                    Some("nvenc") => "NVIDIA NVENC",
                    Some("videotoolbox") => "Apple VideoToolbox",
                    _ => "processor",
                }
            );
        }
        if args.only != Some(Kind::Video) {
            println!(
                "  Images: {} · {} · {}",
                if s.img_format == "webp" { "WebP" } else { "JPEG (WebP if transparent)" },
                s.img_quality,
                if s.img_size == "keep" { "same size".to_string() } else { format!("up to {} px", s.img_size) }
            );
        }
    }

    let mut failures = 0;
    for (dir, picks) in cli_targets(&args.paths) {
        if let Err(e) = media::scan(sh, &dir, picks.clone()) {
            println!("  {e}");
            failures += 1;
            continue;
        }
        let asked = |name: &str| picks.as_ref().map_or(true, |p| p.iter().any(|n| n == name));
        {
            let mut app = lock(sh);
            if let Some(k) = args.only {
                for f in app.files.iter_mut().filter(|f| f.kind != k) {
                    f.selected = false;
                }
            }
            println!("\n  {}", app.dir);
            for f in app.files.iter().filter(|f| !f.ok && asked(&f.name) && args.only.map_or(true, |k| k == f.kind)) {
                println!("  ✗ {}: {}", f.name, f.error.as_deref().unwrap_or("problem"));
                // a whole folder skips bad files quietly; a file named on the command line is a failure
                if picks.is_some() {
                    failures += 1;
                }
            }
            for f in app.files.iter().filter(|f| f.selected && f.status == "done") {
                println!("  ✓ {} (already done)", f.name);
            }
        }
        if media::start(sh).is_err() {
            continue;
        }
        let total = lock(sh).queue.len();
        loop {
            std::thread::sleep(Duration::from_millis(400));
            let app = lock(sh);
            let done = total - app.queue.len();
            let line = match app.current.as_ref().and_then(|cur| app.files.iter().find(|f| &f.name == cur && f.status != "done")) {
                Some(f) => {
                    let width = 24;
                    let filled = ((f.progress * width as f64).round() as usize).min(width);
                    let bar = "█".repeat(filled) + &"░".repeat(width - filled);
                    let proj = if f.progress > 0.02 && f.out_size > 0 { format!(" → ≈{}", fmt_size(f.out_size as f64 / f.progress)) } else { String::new() };
                    let speed = f.speed.map(|s| format!("  {s:.1}×")).unwrap_or_default();
                    let eta = f.eta.map(|e| format!("  {} left", fmt_time(e))).unwrap_or_default();
                    let joining = if f.status == "joining" { "  joining…" } else { "" };
                    let name: String = f.name.chars().take(32).collect();
                    format!("  [{}/{total}] {name}  {bar} {:>3.0}%  {}{proj}{speed}{eta}{joining}", done + 1, f.progress * 100.0, fmt_size(f.size as f64))
                }
                None => format!("  [{done}/{total}] working…"),
            };
            let running = app.running;
            drop(app);
            print!("\r{line:<120}");
            let _ = std::io::stdout().flush();
            if !running {
                break;
            }
        }
        println!("\n");
        let app = lock(sh);
        for f in app.files.iter().filter(|f| f.selected && f.ok) {
            match f.status.as_str() {
                "done" => {
                    let pct = (f.out_size as f64 / f.size.max(1) as f64 - 1.0) * 100.0;
                    let note = f.note.as_deref().map(|n| format!("  ({n})")).unwrap_or_default();
                    println!("  ✓ {}  {} → {}  ({pct:+.0}%){note}", f.name, fmt_size(f.size as f64), fmt_size(f.out_size as f64));
                }
                "error" => {
                    failures += 1;
                    println!("  ✗ {}  {}", f.name, f.error.as_deref().unwrap_or("problem"));
                }
                _ => println!("  … {}  stopped at {:.0}%; run again to carry on", f.name, f.progress * 100.0),
            }
        }
    }
    if failures > 0 { 1 } else { 0 }
}
