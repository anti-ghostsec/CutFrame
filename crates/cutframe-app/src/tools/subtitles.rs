use std::path::{Path, PathBuf};
use std::sync::Arc;

use cutframe_core::subtitles::default_output;
use cutframe_core::{EmbedMode, EmbedRequest, Engine, MediaInfo, SubtitleFormat};
use eframe::egui::{self, Color32, RichText};

use super::common::{
    dropped_files, has_ext, path_label, pick_file, preset_combo, section, JobRunner, OutputPath,
    VIDEO_EXTS,
};
use super::Tool;
use crate::player::Player;

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
pub struct SubtitleTool {
    engine: Arc<dyn Engine>,

    video: Option<PathBuf>,
    video_info: Option<Result<MediaInfo, String>>,
    player: Option<Player>,
    subtitle: Option<PathBuf>,
    output: OutputPath,

    mode: EmbedMode,
    language: String,
    title: String,
    make_default: bool,
    keep_existing: bool,
    crf: u8,
    preset: String,

    job: JobRunner,
}

impl SubtitleTool {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            engine,
            video: None,
            video_info: None,
            player: None,
            subtitle: None,
            output: OutputPath::default(),
            mode: EmbedMode::Soft,
            language: "eng".into(),
            title: String::new(),
            make_default: true,
            keep_existing: true,
            crf: 20,
            preset: "medium".into(),
            job: JobRunner::default(),
        }
    }

    fn set_video(&mut self, path: PathBuf) {
        let info = self.engine.probe(&path).map_err(|e| e.to_string());
        self.player = info.as_ref().ok().filter(|i| i.first_video().is_some()).map(|i| {
            let fps = i.first_video().and_then(|v| v.frame_rate);
            let mut p = Player::new(self.engine.clone(), path.clone(), i.duration, fps);
            p.set_subtitle(self.subtitle.clone());
            p
        });
        self.video_info = Some(info);
        self.video = Some(path);
        self.refresh_output();
    }

    fn set_subtitle(&mut self, path: PathBuf) {
        if self.title.is_empty() {
            if let Some(stem) = path.file_stem() {
                self.title = stem.to_string_lossy().into_owned();
            }
        }
        if let Some(p) = &mut self.player {
            p.set_subtitle(Some(path.clone()));
        }
        self.subtitle = Some(path);
    }

    fn refresh_output(&mut self) {
        if let Some(v) = &self.video {
            self.output.suggest(default_output(v, self.mode));
        }
    }

    fn build_request(&self) -> Option<EmbedRequest> {
        let (video, subtitle) = (self.video.clone()?, self.subtitle.clone()?);
        let mut req = EmbedRequest::new(video, subtitle, self.output.path()?);
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
            self.job.fail(e.to_string());
            return;
        }
        if let Some(p) = &mut self.player {
            p.pause();
        }
        let engine = self.engine.clone();
        self.job.start(ctx, req.output.clone(), move |on_progress, cancel| {
            engine.embed_subtitles(&req, on_progress, cancel)
        });
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
                preset_combo(ui, &mut self.preset);
                ui.end_row();
            }

            ui.label("");
            ui.checkbox(&mut self.keep_existing, "Keep subtitle tracks already in the video");
            ui.end_row();
        });
    }

    fn run_section(&mut self, ui: &mut egui::Ui) {
        if self.job.progress_ui(ui) {
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

        self.job.status_ui(ui);
    }
}

impl Tool for SubtitleTool {
    fn name(&self) -> &'static str {
        "Embed subtitles"
    }

    fn icon(&self) -> &'static str {
        "💬"
    }

    fn description(&self) -> &'static str {
        "Add a subtitle file to a video as a track, or burn it into the picture."
    }

    fn poll(&mut self) {
        self.job.poll();
    }

    fn on_hide(&mut self) {
        if let Some(p) = &mut self.player {
            p.pause();
        }
    }

    fn is_busy(&self) -> bool {
        self.job.is_busy()
    }

    fn open(&mut self, path: &Path) -> bool {
        if SubtitleFormat::from_path(path).is_some() {
            self.set_subtitle(path.to_path_buf());
        } else if has_ext(path, VIDEO_EXTS) {
            self.set_video(path.to_path_buf());
        } else {
            return false;
        }
        true
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.is_busy();
        if !busy {
            for path in dropped_files(ui.ctx()) {
                self.open(&path);
            }
        }

        ui.add_enabled_ui(!busy, |ui| {
            section(ui, "1. Files", |ui| self.file_rows(ui));
            if let Some(player) = &mut self.player {
                section(ui, "Preview", |ui| player.ui(ui));
            }
            section(ui, "2. How to embed", |ui| self.mode_section(ui));
            section(ui, "3. Output", |ui| self.output.ui(ui));
        });
        section(ui, "4. Run", |ui| self.run_section(ui));
    }
}
