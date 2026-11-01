# CutFrame

A cross-platform video tool written in Rust. Right now it can **embed subtitles** and **trim/cut videos**. It's meant to grow into a full video editor for Windows, Linux and Android.

## Features (v0.4)

CutFrame opens on a **Home** screen with a card for each tool. A sidebar on the left switches tools at any time. A video dropped on Home opens in every tool that can use it, and a job keeps running in the background while you use another tool.

### Embed subtitles


- **Soft subtitles**: adds the subtitles as a separate track that viewers can turn on or off. Video and audio are copied as-is, so it's fast and the quality doesn't change.
- **Hard subtitles (burn-in)**: draws the subtitles into the picture so they show on every player and website. This re-encodes the video (H.264, or VP9 for `.webm`).
- Takes `.srt`, `.ass`/`.ssa` and `.vtt` subtitle files.
- Picks the right subtitle codec for the output: `mov_text` for MP4/MOV, copied as-is for MKV, `webvtt` for WebM.
- Sets the subtitle language and title, marks the track as default, and keeps any subtitle tracks the video already has.
- **Preview player:** play, pause (Space) and scrub through the video with the subtitles drawn on top, exactly as burn-in will render them. A checkbox turns the overlay off.

### Trim / Cut

- Mark ranges on the preview with **Set In / Set Out** (keys `I` / `O`, or type times like `1:02.5`). Press **Add range** (`Enter`) to queue several.
- **Keep selected parts** trims the video down to them. **Remove selected parts** cuts them out and keeps the rest. Several ranges are joined in order.
- A timeline bar shows the selection and the playhead. Click it to jump.
- **Fast:** stream copy. Instant and lossless, but cuts snap to keyframes.
- **Precise:** re-encodes to H.264/AAC for frame-exact cuts.

### General

- Supports drag and drop. You can open files from the command line: `cutframe video.mp4 subs.srt`. Add `--trim` or `--subtitles` to skip Home and start in that tool.
- Shows a live progress bar and has a Cancel button.

## Running

You need [Rust](https://rustup.rs) and [FFmpeg](https://ffmpeg.org/download.html) (`ffmpeg` and `ffprobe` must be on `PATH`, or sit next to the `cutframe` executable).

```bash
cargo run --release
```

Try it with the files in `samples/`.

```bash
cargo test            # unit tests + end-to-end tests against real ffmpeg
```

To check the UI without a screen grabber (for scripts and CI), set `CUTFRAME_SCREENSHOT=out.ppm`. `CUTFRAME_SCREENSHOT_AFTER=<secs>` sets how long it waits first, and `CUTFRAME_AUTOPLAY=1` starts the preview playing.

## Project layout

```
crates/
  cutframe-core/   UI-agnostic media logic (no GUI dependencies)
    engine.rs        `Engine` trait: the seam for swapping media backends
    ffmpeg.rs        FfmpegCli: engine that drives ffmpeg/ffprobe executables
    subtitles.rs     EmbedRequest + ffmpeg argument building (pure, unit-tested)
    edit.rs          TimeRange / keep_ranges / TrimRequest (trim & cut)
    preview.rs       PreviewRequest / Frame / FrameSource for on-screen playback
    media.rs         MediaInfo / StreamInfo from probing
    job.rs           Progress + CancelToken for long-running jobs
  cutframe-app/    egui/eframe GUI
    app.rs           Shell: Home screen, sidebar navigation, status bar
    player.rs        Reusable preview widget (decoder thread + frame clock)
    tools/           One module per tool (screen)
      common.rs        Shared pieces: file pickers, output field, JobRunner
      subtitles.rs     Embed subtitles tab
      trim.rs          Trim / Cut tab
```

## Design notes for the full editor

- **Core vs. UI.** All media work goes through `cutframe_core::Engine`. The GUI never builds ffmpeg commands itself, so the backend can change without touching the UI.
- **Engines.** `FfmpegCli` is the simplest way to get going on desktop. Options for later:
  - Link libav* in-process with the `ffmpeg-next` crate. You need this for frame-accurate preview and scrubbing on a timeline.
  - On Android you can't spawn an `ffmpeg` binary from `PATH`. Use one of these:
    - bundle FFmpeg as a native library (`.so`) and implement `Engine` over it
    - use `MediaCodec`/`MediaMuxer` through JNI
- **Preview.** `FfmpegCli` streams scaled frames from ffmpeg as PPM images (each image carries its own size) at up to 30 fps. Subtitles go through the same libass filter as burn-in, so what you see is what you'll export. The preview has no audio yet: the plan is `cpal` playback with the audio as the master clock. For a real timeline, switch the decoder to `ffmpeg-next` so seeking is faster.
- **Tools.** Every feature is a `Tool` (see `tools.rs`). Trim, concat, crop and similar features slot in as new tabs until the timeline editor replaces them.
- **Android.** The GUI is egui/eframe, which runs on Android through `android-activity`. The app code lives in a library (`lib.rs`) so a `cdylib` with `android_main` can reuse it. File pickers are behind `cfg(target_os = "android")` stubs, and they still need the Storage Access Framework.
- **Windows.** Release builds use the GUI subsystem, so no console window appears. FFmpeg child processes start with `CREATE_NO_WINDOW`. Ship `ffmpeg.exe` and `ffprobe.exe` next to `cutframe.exe`.

## Roadmap

1. ~~Embed subtitles (soft/hard)~~
2. ~~Video preview player~~ (audio playback still to do)
3. ~~Trim / cut~~. Still to do: joining separate files (concatenate).
4. Timeline with multiple tracks
5. Subtitle editor (edit timings and text in-app)
6. Android build + packaging (cargo-apk / xbuild); Windows installer
