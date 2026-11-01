//! Trimming and cutting: turn a video into a list of kept time ranges.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Error, Result};

/// Ranges shorter than this are dropped (they'd produce empty segments).
const MIN_RANGE: Duration = Duration::from_millis(40);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Duration,
    pub end: Duration,
}

impl TimeRange {
    pub fn new(a: Duration, b: Duration) -> Self {
        Self { start: a.min(b), end: a.max(b) }
    }

    pub fn len(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() < MIN_RANGE
    }

    pub fn contains(&self, t: Duration) -> bool {
        self.start <= t && t < self.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Selection {
    /// Keep only the selected ranges (trim).
    #[default]
    Keep,
    /// Remove the selected ranges and keep everything else (cut).
    Remove,
}

/// Turn the user's selection into sorted, non-overlapping ranges to keep.
pub fn keep_ranges(selected: &[TimeRange], selection: Selection, duration: Duration) -> Vec<TimeRange> {
    let mut ranges: Vec<TimeRange> = selected
        .iter()
        .map(|r| TimeRange::new(r.start.min(duration), r.end.min(duration)))
        .filter(|r| !r.is_empty())
        .collect();
    ranges.sort_by_key(|r| r.start);

    let mut merged: Vec<TimeRange> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }

    match selection {
        Selection::Keep => merged,
        Selection::Remove => {
            let mut keep = Vec::with_capacity(merged.len() + 1);
            let mut cursor = Duration::ZERO;
            for r in &merged {
                keep.push(TimeRange::new(cursor, r.start));
                cursor = r.end;
            }
            keep.push(TimeRange::new(cursor, duration));
            keep.retain(|r| !r.is_empty());
            keep
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Accuracy {
    /// Copy streams without re-encoding. Near-instant and lossless, but cut
    /// points snap to the nearest earlier keyframe.
    #[default]
    Fast,
    /// Re-encode so cuts land on the exact frame.
    Precise,
}

#[derive(Clone, Debug)]
pub struct TrimRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Ranges to keep, in output order. Build with [`keep_ranges`].
    pub keep: Vec<TimeRange>,
    pub accuracy: Accuracy,
    /// x264 quality for precise mode.
    pub crf: u8,
    pub preset: String,
}

impl TrimRequest {
    pub fn new(input: PathBuf, output: PathBuf, keep: Vec<TimeRange>) -> Self {
        Self { input, output, keep, accuracy: Accuracy::Fast, crf: 18, preset: "medium".into() }
    }

    pub fn output_duration(&self) -> Duration {
        self.keep.iter().map(TimeRange::len).sum()
    }

    pub fn validate(&self) -> Result<()> {
        if !self.input.is_file() {
            return Err(Error::MissingFile(self.input.clone()));
        }
        if crate::subtitles::same_file(&self.input, &self.output) {
            return Err(Error::OutputIsInput);
        }
        if self.keep.iter().all(TimeRange::is_empty) {
            return Err(Error::NothingToKeep);
        }
        Ok(())
    }
}

/// Suggest `<name>.trimmed.<ext>` next to the input.
pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    let ext = input.extension().map(|e| e.to_string_lossy()).unwrap_or("mkv".into());
    input.with_file_name(format!("{stem}.trimmed.{ext}"))
}

/// Concat-demuxer script for fast mode: the same file listed once per range,
/// each with an inpoint/outpoint.
pub(crate) fn concat_script(req: &TrimRequest) -> String {
    let path = req.input.to_string_lossy().replace('\\', "/").replace('\'', r"'\''");
    let mut s = String::from("ffconcat version 1.0\n");
    for r in &req.keep {
        s += &format!(
            "file '{path}'\ninpoint {:.3}\noutpoint {:.3}\n",
            r.start.as_secs_f64(),
            r.end.as_secs_f64()
        );
    }
    s
}

/// ffmpeg arguments. In fast mode `script` is the path of the file written
/// from [`concat_script`]; `has_audio` says whether input has an audio stream.
pub(crate) fn ffmpeg_args(req: &TrimRequest, script: &Path, has_audio: bool) -> Vec<OsString> {
    let strs = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
    let mut a = strs(&["-y", "-hide_banner", "-nostdin", "-progress", "pipe:1", "-nostats"]);

    match req.accuracy {
        Accuracy::Fast => {
            a.extend(strs(&["-f", "concat", "-safe", "0", "-i"]));
            a.push(script.into());
            a.extend(strs(&["-map", "0:v", "-map", "0:a?", "-map", "0:s?", "-c", "copy"]));
            a.extend(strs(&["-avoid_negative_ts", "make_zero"]));
        }
        Accuracy::Precise => {
            // One accurately-seeked input per range, joined with the concat filter.
            let mut pads = String::new();
            for (i, r) in req.keep.iter().enumerate() {
                a.push("-ss".into());
                a.push(format!("{:.3}", r.start.as_secs_f64()).into());
                a.push("-t".into());
                a.push(format!("{:.3}", r.len().as_secs_f64()).into());
                a.push("-i".into());
                a.push(req.input.clone().into());
                pads += &format!("[{i}:v:0]");
                if has_audio {
                    pads += &format!("[{i}:a:0]");
                }
            }
            let n = req.keep.len();
            let (audio, out) = if has_audio { (1, "[v][a]") } else { (0, "[v]") };
            a.push("-filter_complex".into());
            a.push(format!("{pads}concat=n={n}:v=1:a={audio}{out}").into());
            a.extend(strs(&["-map", "[v]"]));
            if has_audio {
                a.extend(strs(&["-map", "[a]", "-c:a", "aac", "-b:a", "192k"]));
            }
            let crf = req.crf.to_string();
            a.extend(strs(&["-c:v", "libx264", "-crf", &crf, "-preset", &req.preset]));
            a.extend(strs(&["-pix_fmt", "yuv420p"]));
        }
    }

    let ext = req.output.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov") {
        a.extend(strs(&["-movflags", "+faststart"]));
    }
    a.push(req.output.clone().into());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }
    fn r(a: f64, b: f64) -> TimeRange {
        TimeRange::new(s(a), s(b))
    }

    #[test]
    fn keep_merges_sorts_and_clamps() {
        let sel = [r(5.0, 8.0), r(1.0, 3.0), r(2.5, 4.0), r(9.0, 20.0)];
        assert_eq!(
            keep_ranges(&sel, Selection::Keep, s(10.0)),
            vec![r(1.0, 4.0), r(5.0, 8.0), r(9.0, 10.0)]
        );
    }

    #[test]
    fn remove_inverts_selection() {
        let sel = [r(2.0, 3.0), r(0.0, 1.0)];
        assert_eq!(keep_ranges(&sel, Selection::Remove, s(5.0)), vec![r(1.0, 2.0), r(3.0, 5.0)]);
        // Removing everything leaves nothing.
        assert!(keep_ranges(&[r(0.0, 5.0)], Selection::Remove, s(5.0)).is_empty());
    }

    #[test]
    fn concat_script_escapes_quotes() {
        let req = TrimRequest::new("/v/it's.mp4".into(), "o.mp4".into(), vec![r(1.0, 2.5)]);
        assert_eq!(
            concat_script(&req),
            "ffconcat version 1.0\nfile '/v/it'\\''s.mp4'\ninpoint 1.000\noutpoint 2.500\n"
        );
    }

    #[test]
    fn precise_args_join_ranges() {
        let mut req =
            TrimRequest::new("in.mp4".into(), "o.mp4".into(), vec![r(1.0, 2.0), r(4.0, 6.0)]);
        req.accuracy = Accuracy::Precise;
        let joined = |audio| {
            ffmpeg_args(&req, Path::new("x"), audio)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let with_audio = joined(true);
        assert!(with_audio.contains("-ss 4.000 -t 2.000 -i in.mp4"), "{with_audio}");
        assert!(with_audio.contains("[0:v:0][0:a:0][1:v:0][1:a:0]concat=n=2:v=1:a=1[v][a]"));
        assert!(joined(false).contains("[0:v:0][1:v:0]concat=n=2:v=1:a=0[v]"));
    }
}
