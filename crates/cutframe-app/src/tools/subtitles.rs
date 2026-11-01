use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use cutframe_core::media::format_duration;
use cutframe_core::subtitles::default_output;
use cutframe_core::{
    CancelToken, EmbedMode, EmbedRequest, Engine, MediaInfo, Progress, SubtitleFormat,
};
use eframe::egui::{self, Color32, RichText};

use super::Tool;

const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "m4v", "webm", "avi", "ts", "flv", "wmv"];
const OUTPUT_EXTS: &[&str] = &["mp4", "mkv", "webm", "mov"];
const LANGUAGES: &[(&str, &str)] = &[
    ("", "(none)"),
    ("eng", "English"),
    ("ara", "Arabic"),
    ("spa", "Spanish"),
    ("fre", "French"),
    ("ger", "German"),
    ("ita", "Italian"),
    ("por", "Portuguese"),
    ("rus", "Russian"),
    ("tur", "Turkish"),
    ("hin", "Hindi"),
    ("jpn", "Japanese"),
    ("kor", "Korean"),
    ("chi", "Chinese"),
];
const PRESETS: &[&str] =
    &["ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower"];

enum JobMsg {
    Progress(Progress),
    Done(Result<(), String>),
}

struct RunningJob {
    rx: Receiver<JobMsg>,
    cancel: CancelToken,
    progress: Option<Progress>,
    started: Instant,
}

enum Status {
    Idle,
    Ok(String),
    Err(String),
}

pub struct SubtitleTool {
    engine: Arc<dyn Engine>,

    video: Option<PathBuf>,
    video_info: Option<Result<MediaInfo, String>>,
    subtitle: Option<PathBuf>,
    output: String,
    /// Once the user edits the output path we stop overwriting it.
    output_touched: bool,

    mode: EmbedMode,
    language: String,
    title: String,
    make_default: bool,
    keep_existing: bool,
    crf: u8,
    preset: String,

    job: Option<RunningJob>,
    status: Status,
}

impl SubtitleTool {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            engine,
            video: None,
            video_info: None,
            subtitle: None,
            output: String::new(),
            output_touched: false,
            mode: EmbedMode::Soft,
            language: "eng".into(),
            title: String::new(),
            make_default: true,
            keep_existing: true,
            crf: 20,
            preset: "medium".into(),
            job: None,
            status: Status::Idle,
        }
    }

    fn set_video(&mut self, path: PathBuf) {
        self.video_info = Some(self.engine.probe(&path).map_err(|e| e.to_string()));
        self.video = Some(path);
        self.refresh_output();
    }

    fn set_subtitle(&mut self, path: PathBuf) {
        if self.title.is_empty() {
            if let Some(stem) = path.file_stem() {
                self.title = stem.to_string_lossy().into_owned();
            }
        }
        self.subtitle = Some(path);
    }

    fn refresh_output(&mut self) {
        if self.output_touched {
            return;
        }
        if let Some(v) = &self.video {
            self.output = default_output(v, self.mode).display().to_string();
        }
    }

    /// Route dropped files by extension.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect()
        });
        for path in dropped {
            if SubtitleFormat::from_path(&path).is_some() {
                self.set_subtitle(path);
            } else if has_ext(&path, VIDEO_EXTS) {
                self.set_video(path);
            }
        }
    }

    fn build_request(&self) -> Option<EmbedRequest> {
        let (video, subtitle) = (self.video.clone()?, self.subtitle.clone()?);
        if self.output.trim().is_empty() {
            return None;
        }
        let mut req = EmbedRequest::new(video, subtitle, PathBuf::from(self.output.trim()));
        req.mode = self.mode;
        req.language = Some(self.language.clone()).filter(|l| !l.is_empty());
        req.title = Some(self.title.trim().to_owned()).filter(|t| !t.is_empty());
        req.make_default = self.make_default;
        req.keep_existing = self.keep_existing;
        req.crf = self.crf;
        req.preset = self.preset.clone();
        Some(req)
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(req) = self.build_request() else { return };
        if let Err(e) = req.validate() {
            self.status = Status::Err(e.to_string());
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let (engine, token, ctx) = (self.engine.clone(), cancel.clone(), ctx.clone());

        thread::spawn(move || {
            let mut on_progress = |p| {
                let _ = tx.send(JobMsg::Progress(p));
                ctx.request_repaint();
            };
            let result = engine
                .embed_subtitles(&req, &mut on_progress, &token)
                .map_err(|e| e.to_string());
            let _ = tx.send(JobMsg::Done(result));
            ctx.request_repaint();
        });

        self.status = Status::Idle;
        self.job = Some(RunningJob { rx, cancel, progress: None, started: Instant::now() });
    }

    fn poll_job(&mut self) {
        let Some(job) = &mut self.job else { return };
        let mut finished = None;
        while let Ok(msg) = job.rx.try_recv() {
            match msg {
                JobMsg::Progress(p) => job.progress = Some(p),
                JobMsg::Done(r) => finished = Some(r),
            }
        }
        if let Some(result) = finished {
            let took = format_duration(job.started.elapsed());
            self.status = match result {
                Ok(()) => Status::Ok(format!("Done in {took} → {}", self.output.trim())),
                Err(e) => Status::Err(e),
            };
            self.job = None;
        }
    }

    fn file_rows(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("files").num_columns(3).spacing([8.0, 8.0]).show(ui, |ui| {
            ui.label("Video");
            ui.label(path_label(self.video.as_deref(), "Drop a video here or browse…"));
            if ui.button("Browse…").clicked() {
                if let Some(p) = pick_file("Video", VIDEO_EXTS) {
                    self.set_video(p);
                }
            }
            ui.end_row();

            ui.label("");
            match &self.video_info {
                Some(Ok(info)) => ui.small(info.summary()),
                Some(Err(e)) => ui.small(RichText::new(e).color(Color32::LIGHT_RED)),
                None => ui.small(""),
            };
            ui.label("");
            ui.end_row();

            ui.label("Subtitles");
            ui.label(path_label(self.subtitle.as_deref(), "Drop a .srt / .ass / .vtt file…"));
            if ui.button("Browse…").clicked() {
                if let Some(p) = pick_file("Subtitles", SubtitleFormat::EXTENSIONS) {
                    self.set_subtitle(p);
                }
            }
            ui.end_row();
        });
    }

    fn mode_section(&mut self, ui: &mut egui::Ui) {
        let before = self.mode;
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.mode, EmbedMode::Soft, "Soft (subtitle track)");
            ui.radio_value(&mut self.mode, EmbedMode::Hard, "Hard (burn into picture)");
        });
        ui.small(match self.mode {
            EmbedMode::Soft => {
                "Fast, no quality loss. Viewers can turn subtitles on/off in their player."
            }
            EmbedMode::Hard => {
                "Always visible on any player or site. Re-encodes the video, so it takes longer."
            }
        });
        if self.mode != before {
            self.refresh_output();
        }
        ui.add_space(6.0);

        egui::Grid::new("opts").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            if self.mode == EmbedMode::Soft {
                ui.label("Language");
                let shown = LANGUAGES
                    .iter()
                    .find(|(c, _)| *c == self.language)
                    .map(|(_, n)| *n)
                    .unwrap_or("(none)");
                egui::ComboBox::from_id_salt("lang").selected_text(shown).show_ui(ui, |ui| {
                    for (code, name) in LANGUAGES {
                        ui.selectable_value(&mut self.language, (*code).to_owned(), *name);
                    }
                });
                ui.end_row();

                ui.label("Track title");
                ui.text_edit_singleline(&mut self.title);
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut self.make_default, "Show by default");
                ui.end_row();
            } else {
                ui.label("Quality (CRF)");
                ui.add(egui::Slider::new(&mut self.crf, 14..=32).text("lower = better, bigger"));
                ui.end_row();

                ui.label("Speed");
                egui::ComboBox::from_id_salt("preset").selected_text(&self.preset).show_ui(
                    ui,
                    |ui| {
                        for p in PRESETS {
                            ui.selectable_value(&mut self.preset, (*p).to_owned(), *p);
                        }
                    },
                );
                ui.end_row();
            }

            ui.label("");
            ui.checkbox(&mut self.keep_existing, "Keep subtitle tracks already in the video");
            ui.end_row();
        });
    }

    fn output_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Save to");
            let w = ui.available_width() - 90.0;
            let edit = ui.add(egui::TextEdit::singleline(&mut self.output).desired_width(w));
            if edit.changed() {
                self.output_touched = true;
            }
            if ui.button("Save as…").clicked() {
                if let Some(p) = save_file(&self.output) {
                    self.output = p.display().to_string();
                    self.output_touched = true;
                }
            }
        });
        if Path::new(self.output.trim()).exists() {
            ui.small(RichText::new("This file exists and will be overwritten.").color(Color32::YELLOW));
        }
    }

    fn run_section(&mut self, ui: &mut egui::Ui) {
        if let Some(job) = &self.job {
            let fraction = job.progress.and_then(|p| p.fraction());
            let mut text = match fraction {
                Some(f) => format!("{:.0}%", f * 100.0),
                None => "Working…".into(),
            };
            if let Some(speed) = job.progress.and_then(|p| p.speed) {
                text += &format!("  ·  {speed:.1}x");
            }
            ui.add(egui::ProgressBar::new(fraction.unwrap_or(0.0)).text(text).animate(true));
            if ui.button("Cancel").clicked() {
                job.cancel.cancel();
            }
            return;
        }

        let ready = self.build_request().is_some();
        let label = match self.mode {
            EmbedMode::Soft => "Add subtitles",
            EmbedMode::Hard => "Burn subtitles",
        };
        if ui.add_enabled(ready, egui::Button::new(RichText::new(label).strong())).clicked() {
            self.start(ui.ctx());
        }
        if !ready {
            ui.small("Choose a video and a subtitle file to start.");
        }

        match &self.status {
            Status::Idle => {}
            Status::Ok(msg) => {
                ui.label(RichText::new(msg).color(Color32::LIGHT_GREEN));
            }
            Status::Err(msg) => {
                ui.label(RichText::new("Failed").color(Color32::LIGHT_RED).strong());
                egui::ScrollArea::vertical().max_height(160.0).id_salt("err").show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(msg).monospace().small()).wrap());
                });
            }
        }
    }
}

impl Tool for SubtitleTool {
    fn name(&self) -> &'static str {
        "Embed subtitles"
    }

    fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll_job();
        let busy = self.is_busy();
        if !busy {
            self.handle_drops(ui.ctx());
        }

        ui.add_enabled_ui(!busy, |ui| {
            section(ui, "1. Files", |ui| self.file_rows(ui));
            section(ui, "2. How to embed", |ui| self.mode_section(ui));
            section(ui, "3. Output", |ui| self.output_row(ui));
        });
        section(ui, "4. Run", |ui| self.run_section(ui));
    }
}

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong());
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        body(ui);
    });
}

fn path_label(path: Option<&Path>, placeholder: &str) -> RichText {
    match path {
        Some(p) => RichText::new(p.file_name().unwrap_or(p.as_os_str()).to_string_lossy()),
        None => RichText::new(placeholder).weak().italics(),
    }
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(not(target_os = "android"))]
fn pick_file(kind: &str, exts: &[&str]) -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter(kind, exts).add_filter("All files", &["*"]).pick_file()
}

#[cfg(not(target_os = "android"))]
fn save_file(current: &str) -> Option<PathBuf> {
    let current = Path::new(current.trim());
    let mut dlg = rfd::FileDialog::new().add_filter("Video", OUTPUT_EXTS);
    if let Some(dir) = current.parent().filter(|d| d.is_dir()) {
        dlg = dlg.set_directory(dir);
    }
    if let Some(name) = current.file_name() {
        dlg = dlg.set_file_name(name.to_string_lossy());
    }
    dlg.save_file()
}

// TODO(android): use the Storage Access Framework picker via JNI.
#[cfg(target_os = "android")]
fn pick_file(_kind: &str, _exts: &[&str]) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "android")]
fn save_file(_current: &str) -> Option<PathBuf> {
    None
}
