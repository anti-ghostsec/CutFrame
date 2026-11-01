//! Engine that drives the `ffmpeg` / `ffprobe` command-line tools.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::engine::Engine;
use crate::job::{CancelToken, Progress};
use crate::media::{MediaInfo, StreamInfo, StreamKind};
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_json() {
        let json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360},
            {"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}},
            {"index":2,"codec_type":"subtitle","codec_name":"subrip"}],
            "format":{"format_name":"matroska,webm","duration":"12.500000"}}"#;
        let info = parse_probe(json).unwrap();
        assert_eq!(info.count(StreamKind::Subtitle), 1);
        assert_eq!(info.duration, Some(Duration::from_millis(12_500)));
        assert_eq!(info.streams[1].language.as_deref(), Some("eng"));
    }
}
