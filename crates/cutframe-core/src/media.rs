use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Clone, Debug)]
pub struct StreamInfo {
    pub index: usize,
    pub kind: StreamKind,
    pub codec: String,
    pub language: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub format: String,
    pub duration: Option<Duration>,
    pub streams: Vec<StreamInfo>,
}

impl MediaInfo {
    pub fn count(&self, kind: StreamKind) -> usize {
        self.streams.iter().filter(|s| s.kind == kind).count()
    }

    pub fn first_video(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.kind == StreamKind::Video)
    }

    /// One-line description for the UI, e.g. "1920x1080 h264 · 2 audio · 0:42".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(v) = self.first_video() {
            match (v.width, v.height) {
                (Some(w), Some(h)) => parts.push(format!("{w}x{h} {}", v.codec)),
                _ => parts.push(v.codec.clone()),
            }
        }
        let audio = self.count(StreamKind::Audio);
        if audio > 0 {
            parts.push(format!("{audio} audio"));
        }
        let subs = self.count(StreamKind::Subtitle);
        if subs > 0 {
            parts.push(format!("{subs} subtitle"));
        }
        if let Some(d) = self.duration {
            parts.push(format_duration(d));
        }
        parts.join(" · ")
    }
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
