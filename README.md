# Shrinker

**Make big videos and photos small.** Shrinker compresses CapCut exports, phone videos,
screen recordings and photos with [ffmpeg](https://ffmpeg.org), from a simple page in your
browser or straight from the terminal. It runs on **Windows, macOS and Linux**.

- **Stop any time and carry on later**, even after closing it or restarting the computer.
  At most the last ~10 seconds of video work are redone.
- **Originals are never changed.** Results go into a `compressed` folder next to them.
- **Fast:** uses the computer's graphics chip for video when it can (Intel Quick Sync,
  NVIDIA NVENC, Apple VideoToolbox), and shrinks several photos at once.
- **Photos come out the right way up** (the phone's rotation is applied), with camera and
  **GPS location data removed**. Transparent images stay transparent.
- Damaged, empty or unsupported files are flagged with a plain reason and skipped. One bad
  file never stops the rest.

Typical results: a 166 MB phone video becomes about 62 MB, and a 4.6 MB photo becomes about
1.7 MB, with no visible difference at the default settings.

---

## Install

Download the file for your computer from the
[**latest release**](https://github.com/kelibst/shrinker/releases/latest).

### Windows 10 / 11

1. Download **`Shrinker-Setup-<version>.exe`** and run it.
   - Windows may say *"Windows protected your PC"* because the installer isn't signed:
     click **More info → Run anyway**.
   - It installs just for you (no administrator password needed).
2. Leave **"Install ffmpeg now"** ticked on the last page if it's offered. It uses
   `winget` to fetch ffmpeg, which Shrinker needs.
3. Open **Shrinker** from the Start menu or the desktop.

The installer can also add **Shrinker** to the right-click **Send to** menu (right-click a
folder or video → *Send to* → *Shrinker*) and put `shrinker` on your PATH for terminal use.
To remove it, go to *Settings → Apps → Shrinker → Uninstall*.

*No installer wanted?* Use `shrinker-<version>-windows-x64-portable.zip`: unzip it anywhere,
even a USB stick, and run `shrinker.exe`. Put `ffmpeg.exe` and `ffprobe.exe` next to it if
the computer doesn't have ffmpeg.

### macOS 11 or later (Apple chip or Intel)

1. Install ffmpeg. With [Homebrew](https://brew.sh): `brew install ffmpeg`
2. Download **`Shrinker-<version>-macos.dmg`**, open it and drag **Shrinker** into
   **Applications**.
3. The first time, **right-click Shrinker → Open → Open**. macOS asks because the app isn't
   notarised by Apple. After that, open it normally.

Shrinker opens in your browser and has no Dock icon; use **Quit** on its page to close it.
To remove it, drag Shrinker from Applications to the Bin.

### Linux: Ubuntu, Debian, Mint, Pop!_OS and other `.deb` systems

```sh
sudo apt install ./shrinker_<version>-1_amd64.deb     # use the arm64 file on ARM machines
```

This also installs ffmpeg. Shrinker then appears in your applications menu. To remove it,
run `sudo apt remove shrinker`.

### Linux: any other distribution (Fedora, Arch, openSUSE…)

```sh
sudo dnf install ffmpeg            # or: sudo pacman -S ffmpeg / sudo zypper install ffmpeg
tar -xzf shrinker-<version>-linux-x64.tar.gz
sudo install -m 755 shrinker /usr/local/bin/
shrinker
```

The binary is static, so it runs on any 64-bit Linux. For the system folder picker, install
`zenity` (GNOME) or `kdialog` (KDE). Without either, Shrinker asks you to type the folder
path instead.

### Phones and tablets

Shrinker is a desktop program. To shrink phone videos, copy them to a computer (or open
Shrinker's page from the computer it runs on).

---

## Use it

### In the browser (the normal way)

Open Shrinker. A page appears with your Videos folder (Movies on macOS).

1. **Open folder…** or **Pick files…** to choose what to shrink. Tick and untick files; the
   **All / Videos / Images** tabs filter the list.
2. Choose the settings:

   **Videos**

   | Setting | Choices |
   | --- | --- |
   | File size | Smallest · **Balanced** · Best look |
   | Format | **H.265** (about half the size) · H.264 (plays on every device) |
   | Resolution | **Keep** · 1080p · 720p (portrait videos work too) |
   | Encoder | **Graphics chip** (many times faster) · Processor |

   **Images**

   | Setting | Choices |
   | --- | --- |
   | File size | Smallest · **Balanced** · Best look |
   | Format | **JPEG** (opens everywhere; transparent images become WebP) · WebP (about 30% smaller) |
   | Largest side | **Keep** · 2560 · 1920 · 1280 pixels |

3. Press **Start**. Each file shows its progress, its new size and how much it saved. The top
   shows totals, speed and time left.
4. **Stop** whenever you like. **Start** (or **Resume**) carries on from where it stopped,
   even days later. **Open output folder** shows the results.

If a photo is already as small as it can usefully get, Shrinker copies the original instead
of making a bigger "compressed" file.

Opening Shrinker again while it's running (another double-click, or *Send to*) just brings
up the running copy with the new folder.

### In the terminal (servers, SSH, scripts)

```sh
shrinker --cli ~/Videos                          # everything in a folder
shrinker --cli trip.mp4 IMG_2041.jpg             # particular files
shrinker --cli ~/Pictures --only images --img-size 1920
shrinker --cli /data/exports --quality small --res 720 --codec h264
```

Press **Ctrl+C** to stop, and run the same command again to carry on. The exit code is
non-zero if a file you named failed. `shrinker --help` lists every option:

| Option | Values | Default |
| --- | --- | --- |
| `--quality` | `small`, `balanced`, `high` (videos) | `balanced` |
| `--codec` | `hevc`, `h264` | `hevc` |
| `--res` | `keep`, `1080`, `720`, any short-side height | `keep` |
| `--encoder` | `auto`, `gpu`, `cpu` | `auto` |
| `--img-quality` | `small`, `balanced`, `high` | `balanced` |
| `--img-format` | `auto` (JPEG, WebP if transparent), `webp` | `auto` |
| `--img-size` | `keep`, any long-side size in pixels | `keep` |
| `--only` | `videos`, `images` | both |
| `--port` | first port to try for the page | `4790` |
| `--no-browser` | don't open the browser | |

---

## How it works

- **Videos** are encoded in 10-second chunks under `compressed/.parts/<video>/`, with a
  `state.json` recording the settings and how far it got. On restart Shrinker keeps every
  finished chunk, counts their frames, and starts the original again at exactly the next
  frame. All chunks share one continuous timeline, so at the end they are joined
  byte-for-byte into the final `.mp4` (no second encode) and the chunks are deleted. A
  stopped video keeps the settings it started with; use **Start over** on it to change them.
- **Images** go through ffmpeg too: JPEG (`mjpeg`) or WebP (`libwebp`). The rotation tag is
  applied to the pixels, and all metadata is stripped.
- The page is served on `127.0.0.1` only, so nothing on your network can reach it.

Known limit: where you pressed Stop, a video's audio can have a gap of about 1/20 s. Videos
that are never stopped don't have it.

### Graphics-chip support

| System | Used when available | Notes |
| --- | --- | --- |
| Windows | NVIDIA NVENC, Intel Quick Sync | Tested on Intel Quick Sync |
| macOS | Apple VideoToolbox | Needs an Apple-chip Mac; Intel Macs use the processor |
| Linux | NVIDIA NVENC, Intel Quick Sync | AMD/VA-API isn't supported yet; uses the processor |

---

## Build from source

Needs [Rust](https://rustup.rs) and ffmpeg.

```sh
git clone https://github.com/kelibst/shrinker
cd shrinker
cargo build --release        # → target/release/shrinker (shrinker.exe on Windows)
cargo test
```

Packaging (the CI workflow does all of this; see `.github/workflows/release.yml`):

| Package | Command | Needs |
| --- | --- | --- |
| Windows installer | `iscc /DVersion=0.2.0 packaging\windows\shrinker.iss` | [Inno Setup 6](https://jrsoftware.org/isinfo.php) |
| macOS app + `.dmg` | `packaging/macos/build-app.sh target/release/shrinker 0.2.0` | a Mac |
| Linux `.deb` | `cargo deb` | `cargo install cargo-deb` |
| Icons | `python packaging/icons/make_icons.py` | Pillow |

### Releasing a new version

1. Bump `version` in `Cargo.toml` and run `cargo build` (this updates `Cargo.lock`), then commit.
2. Tag and push: `git tag v0.3.0 && git push origin v0.3.0`
3. GitHub Actions builds every installer and publishes them on the Releases page (about 10 minutes).

---

MIT licence. Shrinker calls ffmpeg as a separate program and doesn't include it; ffmpeg has
its own licence (LGPL/GPL).
