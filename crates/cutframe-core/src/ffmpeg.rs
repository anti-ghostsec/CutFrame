//! Engine that drives the `ffmpeg` / `ffprobe` command-line tools.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Deserialize;

use crate::engine::Engine;
use crate::job::{CancelToken, Progress};
use crate::media::{MediaInfo, StreamInfo, StreamKind};
use crate::preview::{self, Frame, FrameSource, PreviewRequest};
use crate::subtitles::{ffmpeg_args, EmbedRequest};
use crate::{Error, Result};

/// How many trailing stderr lines to keep for error messages.
const LOG_TAIL: usize = 30;

#[derive(Clone, Debug)]
pub struct FfmpegCli {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Default for FfmpegCli {
    fn default() -> Self {
        Self::locate()
    }
}

impl FfmpegCli {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self { ffmpeg: ffmpeg.into(), ffprobe: ffprobe.into() }
    }

    /// Prefer binaries shipped next to our executable (how we'll bundle FFmpeg
    /// on Windows), otherwise fall back to whatever is on PATH.
    pub fn locate() -> Self {
        let exe = |name: &str| format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let bundled = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let pick = |name: &str| -> PathBuf {
            if let Some(dir) = &bundled {
                let p = dir.join(exe(name));
                if p.is_file() {
                    return p;
                }
            }
            PathBuf::from(exe(name))
        };
        Self::new(pick("ffmpeg"), pick("ffprobe"))
    }

    /// Returns the first line of `ffmpeg -version`, or an error if it can't run.
    pub fn version(&self) -> Result<String> {
        let out = command(&self.ffmpeg)
            .arg("-version")
            .output()
            .map_err(|e| spawn_error(&self.ffmpeg, e))?;
        Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().to_owned())
    }
}

impl Engine for FfmpegCli {
    fn name(&self) -> String {
        self.version().unwrap_or_else(|_| "FFmpeg (not found)".into())
    }

    fn probe(&self, path: &Path) -> Result<MediaInfo> {
        if !path.is_file() {
            return Err(Error::MissingFile(path.to_path_buf()));
        }
        let out = command(&self.ffprobe)
            .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
            .arg(path)
            .output()
            .map_err(|e| spawn_error(&self.ffprobe, e))?;
        if !out.status.success() {
            return Err(Error::Probe(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
        }
        parse_probe(&out.stdout)
    }

    fn embed_subtitles(
        &self,
        req: &EmbedRequest,
        on_progress: &mut dyn FnMut(Progress),
        cancel: &CancelToken,
    ) -> Result<()> {
        req.validate()?;
        let info = self.probe(&req.video)?;
        let args = ffmpeg_args(req, info.count(StreamKind::Subtitle));

        let mut child = command(&self.ffmpeg)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| spawn_error(&self.ffmpeg, e))?;

        // Drain stderr on its own thread so ffmpeg never blocks on a full pipe.
        let stderr = child.stderr.take().expect("stderr piped");
        let log = thread::spawn(move || tail_lines(stderr, LOG_TAIL));

        let stdout = child.stdout.take().expect("stdout piped");
        let result = read_progress(stdout, info.duration, on_progress, cancel, &mut child);

        let status = child.wait()?;
        let log = log.join().unwrap_or_default();

        if cancel.is_cancelled() {
            let _ = std::fs::remove_file(&req.output);
            return Err(Error::Cancelled);
        }
        result?;
        if !status.success() {
            let _ = std::fs::remove_file(&req.output);
            return Err(Error::Ffmpeg { code: status.code(), log });
        }
        Ok(())
    }

    fn open_preview(&self, req: &PreviewRequest) -> Result<Box<dyn FrameSource>> {
        if !req.video.is_file() {
            return Err(Error::MissingFile(req.video.clone()));
        }
        let mut child = command(&self.ffmpeg)
            .args(preview::ffmpeg_args(req))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| spawn_error(&self.ffmpeg, e))?;
        let stderr = child.stderr.take().expect("stderr piped");
        let stdout = BufReader::with_capacity(1 << 20, child.stdout.take().expect("stdout piped"));
        Ok(Box::new(FfmpegFrames {
            child,
            stdout,
            log: Some(thread::spawn(move || tail_lines(stderr, LOG_TAIL))),
            start: req.start,
            frame_time: 1.0 / req.fps,
            decoded: 0,
        }))
    }
}

/// Frames read from an `ffmpeg ... -f image2pipe -c:v ppm pipe:1` process.
struct FfmpegFrames {
    child: Child,
    stdout: BufReader<ChildStdout>,
    log: Option<JoinHandle<String>>,
    start: Duration,
    frame_time: f64,
    decoded: u64,
}

impl FrameSource for FfmpegFrames {
    fn next_frame(&mut self) -> Result<Option<Frame>> {
        match preview::read_ppm(&mut self.stdout) {
            Ok(Some((width, height, rgb))) => {
                // The fps filter spaces output frames evenly from the seek point.
                let pts = self.start + Duration::from_secs_f64(self.decoded as f64 * self.frame_time);
                self.decoded += 1;
                Ok(Some(Frame { pts, width, height, rgb }))
            }
            Ok(None) => {
                let status = self.child.wait()?;
                let log = self.log.take().and_then(|h| h.join().ok()).unwrap_or_default();
                if status.success() {
                    Ok(None)
                } else {
                    Err(Error::Ffmpeg { code: status.code(), log })
                }
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for FfmpegFrames {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_progress(
    stdout: impl Read,
    total: Option<Duration>,
    on_progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
    child: &mut Child,
) -> Result<()> {
    let mut p = Progress { position: Duration::ZERO, total, speed: None };
    for line in BufReader::new(stdout).lines() {
        if cancel.is_cancelled() {
            let _ = child.kill();
            break;
        }
        let line = line?;
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            // Despite the name, out_time_ms is also microseconds.
            "out_time_us" | "out_time_ms" => {
                if let Ok(us) = value.trim().parse::<u64>() {
                    p.position = Duration::from_micros(us);
                }
            }
            "speed" => p.speed = value.trim().trim_end_matches('x').parse().ok(),
            "progress" => {
                if value == "end" {
                    if let Some(t) = total {
                        p.position = t;
                    }
                }
                on_progress(p);
            }
            _ => {}
        }
    }
    Ok(())
}

fn tail_lines(r: impl Read, n: usize) -> String {
    let mut buf = VecDeque::with_capacity(n);
    for line in BufReader::new(r).lines().map_while(|l| l.ok()) {
        if buf.len() == n {
            buf.pop_front();
        }
        buf.push_back(line);
    }
    Vec::from(buf).join("\n")
}

fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        // Don't flash a console window when launched from the GUI.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn spawn_error(program: &Path, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::ToolNotFound(program.display().to_string())
    } else {
        Error::Io(e)
    }
}

#[derive(Deserialize)]
struct ProbeOut {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: usize,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

fn parse_probe(json: &[u8]) -> Result<MediaInfo> {
    let p: ProbeOut = serde_json::from_slice(json).map_err(|e| Error::Probe(e.to_string()))?;
    let streams = p
        .streams
        .into_iter()
        .map(|s| StreamInfo {
            index: s.index,
            kind: match s.codec_type.as_deref() {
                Some("video") => StreamKind::Video,
                Some("audio") => StreamKind::Audio,
                Some("subtitle") => StreamKind::Subtitle,
                _ => StreamKind::Other,
            },
            codec: s.codec_name.unwrap_or_else(|| "unknown".into()),
            language: s.tags.get("language").cloned(),
            width: s.width,
            height: s.height,
            frame_rate: parse_rate(s.avg_frame_rate.as_deref())
                .or_else(|| parse_rate(s.r_frame_rate.as_deref())),
        })
        .collect();
    let (format, duration) = match p.format {
        Some(f) => (
            f.format_name.unwrap_or_default(),
            f.duration
                .and_then(|d| d.parse::<f64>().ok())
                .filter(|d| d.is_finite() && *d >= 0.0)
                .map(Duration::from_secs_f64),
        ),
        None => (String::new(), None),
    };
    Ok(MediaInfo { format, duration, streams })
}

/// Parse ffprobe rates like "30000/1001". "0/0" means unknown.
fn parse_rate(r: Option<&str>) -> Option<f64> {
    let (n, d) = r?.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    let fps = n / d;
    (fps.is_finite() && fps > 0.0).then_some(fps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_json() {
        let json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360,"avg_frame_rate":"30000/1001"},
            {"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}},
            {"index":2,"codec_type":"subtitle","codec_name":"subrip"}],
            "format":{"format_name":"matroska,webm","duration":"12.500000"}}"#;
        let info = parse_probe(json).unwrap();
        assert_eq!(info.count(StreamKind::Subtitle), 1);
        assert_eq!(info.duration, Some(Duration::from_millis(12_500)));
        assert_eq!(info.streams[1].language.as_deref(), Some("eng"));
        assert!((info.streams[0].frame_rate.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_rate(Some("0/0")), None);
    }
}
