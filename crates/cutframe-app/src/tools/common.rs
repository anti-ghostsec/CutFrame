//! Building blocks shared by tools: file pickers, output path field, and a
//! background job runner with progress/cancel/status UI.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

use cutframe_core::media::format_duration;
use cutframe_core::{CancelToken, Progress};
use eframe::egui::{self, Color32, RichText};

pub const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "m4v", "webm", "avi", "ts", "flv", "wmv"];
pub const OUTPUT_EXTS: &[&str] = &["mp4", "mkv", "webm", "mov"];
pub const PRESETS: &[&str] =
    &["ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower"];

pub fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong());
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        body(ui);
    });
}

pub fn path_label(path: Option<&Path>, placeholder: &str) -> RichText {
    match path {
        Some(p) => RichText::new(p.file_name().unwrap_or(p.as_os_str()).to_string_lossy()),
        None => RichText::new(placeholder).weak().italics(),
    }
}

pub fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn dropped_files(ctx: &egui::Context) -> Vec<PathBuf> {
    ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect())
}

pub fn preset_combo(ui: &mut egui::Ui, preset: &mut String) {
    egui::ComboBox::from_id_salt("preset").selected_text(preset.as_str()).show_ui(ui, |ui| {
        for p in PRESETS {
            ui.selectable_value(preset, (*p).to_owned(), *p);
        }
    });
}

/// "Save to" field that follows a suggested path until the user edits it.
#[derive(Default)]
pub struct OutputPath {
    text: String,
    touched: bool,
}

impl OutputPath {
    pub fn suggest(&mut self, path: PathBuf) {
        if !self.touched {
            self.text = path.display().to_string();
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        let t = self.text.trim();
        (!t.is_empty()).then(|| PathBuf::from(t))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Save to");
            let w = ui.available_width() - 90.0;
            if ui.add(egui::TextEdit::singleline(&mut self.text).desired_width(w)).changed() {
                self.touched = true;
            }
            if ui.button("Save as…").clicked() {
                if let Some(p) = save_file(&self.text) {
                    self.text = p.display().to_string();
                    self.touched = true;
                }
            }
        });
        if Path::new(self.text.trim()).exists() {
            ui.small(RichText::new("This file exists and will be overwritten.").color(Color32::YELLOW));
        }
    }
}

enum JobMsg {
    Progress(Progress),
    Done(Result<(), String>),
}

struct RunningJob {
    rx: Receiver<JobMsg>,
    cancel: CancelToken,
    progress: Option<Progress>,
    started: Instant,
    output: PathBuf,
}

enum Status {
    Idle,
    Ok(String),
    Err(String),
}

/// Runs one long job at a time on a worker thread.
pub struct JobRunner {
    job: Option<RunningJob>,
    status: Status,
}

impl Default for JobRunner {
    fn default() -> Self {
        Self { job: None, status: Status::Idle }
    }
}

impl JobRunner {
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn fail(&mut self, msg: impl Into<String>) {
        self.status = Status::Err(msg.into());
    }

    /// Start `work` on a background thread. It gets a progress callback and a
    /// cancel token, and writes to `output`.
    pub fn start<F>(&mut self, ctx: &egui::Context, output: PathBuf, work: F)
    where
        F: FnOnce(&mut dyn FnMut(Progress), &CancelToken) -> cutframe_core::Result<()>
            + Send
            + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let cancel = CancelToken::new();
        let (token, ctx) = (cancel.clone(), ctx.clone());
        thread::spawn(move || {
            let mut on_progress = |p| {
                let _ = tx.send(JobMsg::Progress(p));
                ctx.request_repaint();
            };
            let result = work(&mut on_progress, &token).map_err(|e| e.to_string());
            let _ = tx.send(JobMsg::Done(result));
            ctx.request_repaint();
        });
        self.status = Status::Idle;
        self.job = Some(RunningJob { rx, cancel, progress: None, started: Instant::now(), output });
    }

    /// Collect messages from the worker. Call once per frame.
    pub fn poll(&mut self) {
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
                Ok(()) => Status::Ok(format!("Done in {took} → {}", job.output.display())),
                Err(e) => Status::Err(e),
            };
            self.job = None;
        }
    }

    /// Progress bar + Cancel while running. Returns true if a job is running.
    pub fn progress_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(job) = &self.job else { return false };
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
        true
    }

    /// Result of the last job.
    pub fn status_ui(&self, ui: &mut egui::Ui) {
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

#[cfg(not(target_os = "android"))]
pub fn pick_file(kind: &str, exts: &[&str]) -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter(kind, exts).add_filter("All files", &["*"]).pick_file()
}

#[cfg(not(target_os = "android"))]
pub fn save_file(current: &str) -> Option<PathBuf> {
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
pub fn pick_file(_kind: &str, _exts: &[&str]) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "android")]
pub fn save_file(_current: &str) -> Option<PathBuf> {
    None
}
