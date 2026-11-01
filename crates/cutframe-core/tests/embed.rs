//! End-to-end tests against the real ffmpeg. Skipped if ffmpeg isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use std::time::Duration;

use cutframe_core::{
    Accuracy, CancelToken, EmbedMode, EmbedRequest, Engine, FfmpegCli, MediaInfo, Selection,
    StreamKind, TimeRange, TrimRequest,
};

const SRT: &str = "1\n00:00:00,200 --> 00:00:01,500\nHello, world!\n\n2\n00:00:01,600 --> 00:00:02,800\nSecond line\n";

fn workdir(name: &str) -> Option<PathBuf> {
    if FfmpegCli::locate().version().is_err() {
        eprintln!("ffmpeg not found, skipping");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("cutframe-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

fn make_video(path: &Path) {
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
        // A keyframe every second so fast (copy) cuts have somewhere to land.
        .args(["-c:v", "libx264", "-g", "25", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not create sample video");
}

fn run(req: &EmbedRequest) -> Vec<f32> {
    let mut seen = Vec::new();
    FfmpegCli::locate()
        .embed_subtitles(req, &mut |p| seen.extend(p.fraction()), &CancelToken::new())
        .unwrap_or_else(|e| panic!("{e}"));
    seen
}

#[test]
fn soft_mp4_adds_tagged_track() {
    let Some(dir) = workdir("soft-mp4") else { return };
    let (video, subs, out) = (dir.join("in.mp4"), dir.join("subs.srt"), dir.join("out.mp4"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    let mut req = EmbedRequest::new(video, subs, out.clone());
    req.language = Some("eng".into());
    let progress = run(&req);
    assert_eq!(progress.last().copied(), Some(1.0));

    let info = FfmpegCli::locate().probe(&out).unwrap();
    let sub = info.streams.iter().find(|s| s.kind == StreamKind::Subtitle).unwrap();
    assert_eq!(sub.codec, "mov_text");
    assert_eq!(sub.language.as_deref(), Some("eng"));
    assert_eq!(info.count(StreamKind::Audio), 1);
}

#[test]
fn soft_mkv_keeps_existing_and_appends() {
    let Some(dir) = workdir("soft-mkv") else { return };
    let (video, subs) = (dir.join("in.mp4"), dir.join("subs.srt"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    let first = dir.join("one.mkv");
    run(&EmbedRequest::new(video, subs.clone(), first.clone()));
    let second = dir.join("two.mkv");
    run(&EmbedRequest::new(first, subs, second.clone()));

    let info = FfmpegCli::locate().probe(&second).unwrap();
    assert_eq!(info.count(StreamKind::Subtitle), 2);
}

#[test]
fn hard_burn_with_awkward_filename() {
    let Some(dir) = workdir("hard") else { return };
    let video = dir.join("in.mp4");
    // Quotes, colons, commas and brackets all need escaping in -vf.
    let subs = dir.join("it's [a]: weird, name.srt");
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    let mut req = EmbedRequest::new(video, subs, dir.join("burned.mp4"));
    req.mode = EmbedMode::Hard;
    req.preset = "ultrafast".into();
    run(&req);

    let info = FfmpegCli::locate().probe(&req.output).unwrap();
    assert_eq!(info.count(StreamKind::Subtitle), 0);
    assert_eq!(info.first_video().unwrap().codec, "h264");
}

#[test]
fn rejects_output_equal_to_input() {
    let Some(dir) = workdir("same") else { return };
    let (video, subs) = (dir.join("in.mp4"), dir.join("subs.srt"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();
    let req = EmbedRequest::new(video.clone(), subs, video);
    let err = FfmpegCli::locate()
        .embed_subtitles(&req, &mut |_| {}, &CancelToken::new())
        .unwrap_err();
    assert!(matches!(err, cutframe_core::Error::OutputIsInput));
}

fn decode_all(req: &cutframe_core::PreviewRequest) -> Vec<cutframe_core::Frame> {
    let mut src = FfmpegCli::locate().open_preview(req).unwrap();
    let mut frames = Vec::new();
    while let Some(f) = src.next_frame().unwrap() {
        frames.push(f);
    }
    frames
}

#[test]
fn preview_streams_scaled_frames() {
    let Some(dir) = workdir("preview") else { return };
    let video = dir.join("in.mp4");
    make_video(&video);

    let mut req = cutframe_core::PreviewRequest::new(video);
    req.max_width = 160;
    req.fps = 10.0;
    let frames = decode_all(&req);
    assert!((59..=61).contains(&frames.len()), "got {} frames", frames.len());
    let f = &frames[0];
    assert_eq!((f.width, f.height), (160, 120));
    assert_eq!(f.rgb.len(), 160 * 120 * 3);
    assert_eq!(frames[10].pts, std::time::Duration::from_secs(1));
}

#[test]
fn preview_after_seek_draws_subtitles() {
    let Some(dir) = workdir("preview-subs") else { return };
    let (video, subs) = (dir.join("in.mp4"), dir.join("subs.srt"));
    make_video(&video);
    std::fs::write(&subs, SRT).unwrap();

    // 2.0s is inside the second cue (1.6–2.8s); seeking must keep timing right.
    let mut req = cutframe_core::PreviewRequest::new(video);
    req.start = std::time::Duration::from_secs(2);
    req.single_frame = true;
    let plain = decode_all(&req);
    req.subtitle = Some(subs);
    let subbed = decode_all(&req);

    assert_eq!((plain.len(), subbed.len()), (1, 1));
    let changed = plain[0].rgb.iter().zip(&subbed[0].rgb).filter(|(a, b)| a != b).count();
    assert!(changed > 500, "subtitles not visible ({changed} bytes differ)");

    // 1.55s falls in the gap between cues, so nothing should be drawn.
    req.start = std::time::Duration::from_millis(1550);
    let gap = decode_all(&req);
    req.subtitle = None;
    let gap_plain = decode_all(&req);
    assert!(gap[0].rgb == gap_plain[0].rgb, "subtitle drawn outside its cue");
}

fn trim(name: &str, sel: &[(f64, f64)], selection: Selection, accuracy: Accuracy) -> Option<MediaInfo> {
    let dir = workdir(name)?;
    let video = dir.join("in.mp4");
    make_video(&video);
    let ranges: Vec<TimeRange> = sel
        .iter()
        .map(|&(a, b)| TimeRange::new(Duration::from_secs_f64(a), Duration::from_secs_f64(b)))
        .collect();
    let keep = cutframe_core::edit::keep_ranges(&ranges, selection, Duration::from_secs(6));
    let mut req = TrimRequest::new(video, dir.join("out.mp4"), keep);
    req.accuracy = accuracy;
    req.preset = "ultrafast".into();
    let mut last = None;
    FfmpegCli::locate()
        .trim(&req, &mut |p| last = p.fraction(), &CancelToken::new())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(last, Some(1.0));
    Some(FfmpegCli::locate().probe(&req.output).unwrap())
}

fn assert_secs(info: &MediaInfo, expected: f64, tolerance: f64) {
    let got = info.duration.unwrap().as_secs_f64();
    assert!((got - expected).abs() <= tolerance, "expected ~{expected}s, got {got}s");
}

#[test]
fn trim_fast_keeps_one_range() {
    let Some(info) = trim("trim-fast", &[(1.0, 3.0)], Selection::Keep, Accuracy::Fast) else { return };
    // Stream copy cuts on packet boundaries, so allow a few frames of slack.
    assert_secs(&info, 2.0, 0.25);
    assert_eq!(info.count(StreamKind::Audio), 1);
}

#[test]
fn trim_fast_joins_several_ranges() {
    let sel = [(1.0, 2.0), (3.0, 5.0)];
    let Some(info) = trim("trim-fast-multi", &sel, Selection::Keep, Accuracy::Fast) else { return };
    assert_secs(&info, 3.0, 0.3);
}

#[test]
fn cut_precise_removes_ranges_frame_accurately() {
    // Off-keyframe cut points: only re-encoding can hit these exactly.
    let sel = [(1.3, 2.1), (4.45, 5.0)];
    let Some(info) = trim("cut-precise", &sel, Selection::Remove, Accuracy::Precise) else { return };
    assert_secs(&info, 6.0 - 0.8 - 0.55, 0.08);
    assert_eq!(info.count(StreamKind::Audio), 1);
    assert_eq!(info.first_video().unwrap().codec, "h264");
}
