use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cutframe_core::edit::{default_output, keep_ranges};
use cutframe_core::media::{format_duration, format_timestamp, parse_timestamp};
use cutframe_core::{Accuracy, Engine, MediaInfo, Selection, TimeRange, TrimRequest};
use eframe::egui::{self, Color32, RichText, Stroke};

use super::common::{
    dropped_files, has_ext, path_label, pick_file, preset_combo, section, JobRunner, OutputPath,
    VIDEO_EXTS,
};
use super::Tool;
use crate::player::Player;

const KEEP_COLOR: Color32 = Color32::from_rgb(70, 160, 90);
const REMOVE_COLOR: Color32 = Color32::from_rgb(190, 70, 70);
const MARK_COLOR: Color32 = Color32::from_rgb(80, 140, 220);

/// An editable In/Out point: the parsed value plus the text being typed.
#[derive(Default)]
struct Mark {
    value: Option<Duration>,
    text: String,
}

impl Mark {
    fn set(&mut self, t: Duration) {
        self.value = Some(t);
        self.text = format_timestamp(t);
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Text field; returns true if the user typed a new valid time.
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let valid = self.text.trim().is_empty() || parse_timestamp(&self.text).is_some();
        let edit = egui::TextEdit::singleline(&mut self.text)
            .desired_width(90.0)
            .hint_text("m:ss.mmm")
            .text_color_opt((!valid).then_some(Color32::LIGHT_RED));
        let resp = ui.add(edit);
        if resp.changed() {
            self.value = parse_timestamp(&self.text);
        }
        if resp.lost_focus() {
            match self.value {
                Some(t) => self.text = format_timestamp(t),
                None => self.text.clear(),
            }
        }
        resp.changed() && self.value.is_some()
    }
}

pub struct TrimTool {
    engine: Arc<dyn Engine>,

    video: Option<PathBuf>,
    video_info: Option<Result<MediaInfo, String>>,
    player: Option<Player>,

    mark_in: Mark,
    mark_out: Mark,
    ranges: Vec<TimeRange>,
    selection: Selection,

    accuracy: Accuracy,
    crf: u8,
    preset: String,

    output: OutputPath,
    job: JobRunner,
}

impl TrimTool {
    pub fn new(engine: Arc<dyn Engine>) -> Self {
        Self {
            engine,
            video: None,
            video_info: None,
            player: None,
            mark_in: Mark::default(),
            mark_out: Mark::default(),
            ranges: Vec::new(),
            selection: Selection::Keep,
            accuracy: Accuracy::Fast,
            crf: 18,
            preset: "medium".into(),
            output: OutputPath::default(),
            job: JobRunner::default(),
        }
    }

    fn set_video(&mut self, path: PathBuf) {
        let info = self.engine.probe(&path).map_err(|e| e.to_string());
        self.player = info.as_ref().ok().filter(|i| i.first_video().is_some()).map(|i| {
            let fps = i.first_video().and_then(|v| v.frame_rate);
            Player::new(self.engine.clone(), path.clone(), i.duration, fps)
        });
        self.output.suggest(default_output(&path));
        self.video_info = Some(info);
        self.video = Some(path);
        self.ranges.clear();
        self.mark_in.clear();
        self.mark_out.clear();
    }

    fn duration(&self) -> Option<Duration> {
        self.player.as_ref().and_then(Player::duration)
    }

    /// The In/Out marks as a range, if both are set and sensible.
    fn pending_range(&self) -> Option<TimeRange> {
        let r = TimeRange::new(self.mark_in.value?, self.mark_out.value?);
        (!r.is_empty()).then_some(r)
    }

    fn add_pending(&mut self) {
        if let Some(r) = self.pending_range() {
            self.ranges.push(r);
            self.ranges.sort_by_key(|r| r.start);
            self.mark_in.clear();
            self.mark_out.clear();
        }
    }

    /// Ranges that will end up in the output. A set-but-not-added In/Out pair
    /// counts too, so a simple trim is just "set In, set Out, Export".
    fn keep(&self) -> Vec<TimeRange> {
        let Some(duration) = self.duration() else { return Vec::new() };
        let mut selected = self.ranges.clone();
        selected.extend(self.pending_range());
        keep_ranges(&selected, self.selection, duration)
    }

    fn build_request(&self) -> Option<TrimRequest> {
        let keep = self.keep();
        if keep.is_empty() {
            return None;
        }
        let mut req = TrimRequest::new(self.video.clone()?, self.output.path()?, keep);
        req.accuracy = self.accuracy;
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
            engine.trim(&req, on_progress, cancel)
        });
    }

    fn file_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Video");
            ui.label(path_label(self.video.as_deref(), "Drop a video here or browse…"));
            if ui.button("Browse…").clicked() {
                if let Some(p) = pick_file("Video", VIDEO_EXTS) {
                    self.set_video(p);
                }
            }
        });
        match &self.video_info {
            Some(Ok(info)) => ui.small(info.summary()),
            Some(Err(e)) => ui.small(RichText::new(e).color(Color32::LIGHT_RED)),
            None => ui.small(""),
        };
    }

    /// Bar showing the whole video with selected ranges, the pending In/Out
    /// and the playhead. Click or drag to seek.
    fn timeline(&mut self, ui: &mut egui::Ui) {
        let (Some(playhead), Some(total)) = (self.player.as_ref().map(Player::position), self.duration())
        else {
            return;
        };
        if total.is_zero() {
            return;
        }
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 26.0),
            egui::Sense::click_and_drag(),
        );
        let x_of = |t: Duration| {
            rect.left() + rect.width() * (t.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
        };
        let span = |r: &TimeRange| {
            egui::Rect::from_x_y_ranges(x_of(r.start)..=x_of(r.end).max(x_of(r.start) + 1.0), rect.y_range())
        };

        let painter = ui.painter_at(rect);
        let base = match self.selection {
            Selection::Keep => Color32::from_gray(45),
            Selection::Remove => KEEP_COLOR.gamma_multiply(0.45),
        };
        painter.rect_filled(rect, 3.0, base);
        let selected_color = match self.selection {
            Selection::Keep => KEEP_COLOR,
            Selection::Remove => REMOVE_COLOR,
        };
        for r in &self.ranges {
            painter.rect_filled(span(r), 0.0, selected_color);
        }
        if let Some(r) = self.pending_range() {
            painter.rect_filled(span(&r), 0.0, MARK_COLOR.gamma_multiply(0.7));
        }
        for t in [self.mark_in.value, self.mark_out.value].into_iter().flatten() {
            let x = x_of(t);
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], Stroke::new(2.0, MARK_COLOR));
        }
        let x = x_of(playhead);
        painter.line_segment([egui::pos2(x, rect.top() - 2.0), egui::pos2(x, rect.bottom() + 2.0)], Stroke::new(2.0, Color32::WHITE));

        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.clicked() || resp.drag_stopped() {
                let f = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                if let Some(p) = &mut self.player {
                    p.seek(total.mul_f32(f));
                }
            }
        }
        resp.on_hover_text("Click to jump here");
    }

    fn select_section(&mut self, ui: &mut egui::Ui) {
        let Some(playhead) = self.player.as_ref().map(Player::position) else {
            ui.small("Load a video first.");
            return;
        };

        ui.horizontal(|ui| {
            ui.radio_value(&mut self.selection, Selection::Keep, "Keep selected parts (trim)");
            ui.radio_value(&mut self.selection, Selection::Remove, "Remove selected parts (cut)");
        });
        ui.add_space(4.0);
        self.timeline(ui);
        ui.add_space(4.0);

        let typing = ui.ctx().egui_wants_keyboard_input();
        let [key_in, key_out, key_add] = [egui::Key::I, egui::Key::O, egui::Key::Enter]
            .map(|k| !typing && ui.input(|i| i.key_pressed(k)));
        ui.horizontal(|ui| {
            if ui.button("Set In").on_hover_text("Mark the playhead as start (I)").clicked() || key_in {
                self.mark_in.set(playhead);
            }
            self.mark_in.ui(ui);
            ui.label("→");
            if ui.button("Set Out").on_hover_text("Mark the playhead as end (O)").clicked() || key_out {
                self.mark_out.set(playhead);
            }
            self.mark_out.ui(ui);
            let can_add = self.pending_range().is_some();
            if ui
                .add_enabled(can_add, egui::Button::new("Add range"))
                .on_hover_text("Add and start another range (Enter)")
                .clicked()
                || (can_add && key_add)
            {
                self.add_pending();
            }
        });

        let mut remove = None;
        let mut jump = None;
        for (i, r) in self.ranges.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.monospace(format!(
                    "{}. {} – {}  ({})",
                    i + 1,
                    format_timestamp(r.start),
                    format_timestamp(r.end),
                    format_duration(r.len())
                ));
                if ui.small_button("Go").on_hover_text("Jump to start").clicked() {
                    jump = Some(r.start);
                }
                if ui.small_button("✖").on_hover_text("Remove this range").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.ranges.remove(i);
        }
        if let (Some(t), Some(p)) = (jump, &mut self.player) {
            p.seek(t);
        }

        let keep = self.keep();
        let total: Duration = keep.iter().map(TimeRange::len).sum();
        ui.add_space(4.0);
        if keep.is_empty() {
            ui.small("Set In and Out to choose a part of the video.");
        } else {
            ui.small(format!(
                "Result: {} in {} part{}",
                format_timestamp(total),
                keep.len(),
                if keep.len() == 1 { "" } else { "s" }
            ));
        }
    }

    fn accuracy_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.accuracy, Accuracy::Fast, "Fast (no re-encode)");
            ui.radio_value(&mut self.accuracy, Accuracy::Precise, "Precise (re-encode)");
        });
        match self.accuracy {
            Accuracy::Fast => {
                ui.small("Instant and lossless, but cuts snap to keyframes, so they can be off by up to a few seconds.");
            }
            Accuracy::Precise => {
                ui.small("Cuts land on the exact frame. Re-encodes the video (H.264 + AAC), so it takes longer.");
                egui::Grid::new("trim_opts").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Quality (CRF)");
                    ui.add(egui::Slider::new(&mut self.crf, 14..=32).text("lower = better, bigger"));
                    ui.end_row();
                    ui.label("Speed");
                    preset_combo(ui, &mut self.preset);
                    ui.end_row();
                });
            }
        }
    }

    fn run_section(&mut self, ui: &mut egui::Ui) {
        if self.job.progress_ui(ui) {
            return;
        }
        let ready = self.build_request().is_some();
        if ui.add_enabled(ready, egui::Button::new(RichText::new("Export").strong())).clicked() {
            self.start(ui.ctx());
        }
        self.job.status_ui(ui);
    }
}

impl Tool for TrimTool {
    fn name(&self) -> &'static str {
        "Trim / Cut"
    }

    fn icon(&self) -> &'static str {
        "✂"
    }

    fn description(&self) -> &'static str {
        "Keep only the parts you want, or cut sections out of a video."
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
        if has_ext(path, VIDEO_EXTS) {
            self.set_video(path.to_path_buf());
            true
        } else {
            false
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.is_busy();
        if !busy {
            for path in dropped_files(ui.ctx()) {
                self.open(&path);
            }
        }

        ui.add_enabled_ui(!busy, |ui| {
            section(ui, "1. Video", |ui| self.file_row(ui));
            if let Some(player) = &mut self.player {
                section(ui, "Preview", |ui| player.ui(ui));
            }
            section(ui, "2. Choose parts", |ui| self.select_section(ui));
            section(ui, "3. Accuracy", |ui| self.accuracy_section(ui));
            section(ui, "4. Output", |ui| self.output.ui(ui));
        });
        section(ui, "5. Export", |ui| self.run_section(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cutframe_core::{
        CancelToken, EmbedRequest, FrameSource, PreviewRequest, Progress, StreamInfo, StreamKind,
    };

    /// Engine that reports a 10s video and never actually runs anything.
    struct FakeEngine;

    impl Engine for FakeEngine {
        fn name(&self) -> String {
            "fake".into()
        }
        fn probe(&self, _: &Path) -> cutframe_core::Result<MediaInfo> {
            Ok(MediaInfo {
                format: "mp4".into(),
                duration: Some(Duration::from_secs(10)),
                streams: vec![StreamInfo {
                    index: 0,
                    kind: StreamKind::Video,
                    codec: "h264".into(),
                    language: None,
                    width: Some(640),
                    height: Some(360),
                    frame_rate: Some(25.0),
                }],
            })
        }
        fn embed_subtitles(
            &self,
            _: &EmbedRequest,
            _: &mut dyn FnMut(Progress),
            _: &CancelToken,
        ) -> cutframe_core::Result<()> {
            unimplemented!()
        }
        fn trim(
            &self,
            _: &TrimRequest,
            _: &mut dyn FnMut(Progress),
            _: &CancelToken,
        ) -> cutframe_core::Result<()> {
            unimplemented!()
        }
        fn open_preview(&self, _: &PreviewRequest) -> cutframe_core::Result<Box<dyn FrameSource>> {
            unimplemented!()
        }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn tool() -> TrimTool {
        let mut t = TrimTool::new(Arc::new(FakeEngine));
        t.set_video("/videos/clip.mp4".into());
        t
    }

    #[test]
    fn simple_trim_uses_marks_without_adding() {
        let mut t = tool();
        assert!(t.build_request().is_none(), "nothing selected yet");
        t.mark_in.set(secs(2));
        t.mark_out.set(secs(5));
        let req = t.build_request().unwrap();
        assert_eq!(req.keep, vec![TimeRange::new(secs(2), secs(5))]);
        assert_eq!(req.output, PathBuf::from("/videos/clip.trimmed.mp4"));
    }

    #[test]
    fn marks_set_in_reverse_still_make_a_range() {
        let mut t = tool();
        t.mark_in.set(secs(7));
        t.mark_out.set(secs(3));
        assert_eq!(t.pending_range(), Some(TimeRange::new(secs(3), secs(7))));
    }

    #[test]
    fn cut_mode_removes_added_ranges() {
        let mut t = tool();
        t.mark_in.set(secs(1));
        t.mark_out.set(secs(2));
        t.add_pending();
        assert!(t.mark_in.value.is_none() && t.mark_out.value.is_none());
        t.mark_in.set(secs(8));
        t.mark_out.set(secs(10));
        t.add_pending();
        t.selection = Selection::Remove;
        assert_eq!(
            t.keep(),
            vec![TimeRange::new(secs(0), secs(1)), TimeRange::new(secs(2), secs(8))]
        );
    }

    #[test]
    fn loading_another_video_resets_selection() {
        let mut t = tool();
        t.mark_in.set(secs(1));
        t.mark_out.set(secs(2));
        t.add_pending();
        t.set_video("/videos/other.mkv".into());
        assert!(t.ranges.is_empty());
        assert!(t.build_request().is_none());
    }
}
