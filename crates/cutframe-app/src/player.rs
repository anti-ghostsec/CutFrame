//! Reusable video preview widget: play/pause, seek bar, optional subtitle
//! overlay. Decoding runs on a background thread; the UI only uploads frames.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cutframe_core::media::format_duration;
use cutframe_core::{Engine, Frame, PreviewRequest};
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions};

/// Frames buffered ahead of the playhead. Small, so seeking stays snappy.
const BUFFER: usize = 4;
const MAX_HEIGHT: f32 = 360.0;

enum Msg {
    Frame(Frame),
    End,
    Error(String),
}

/// A running decoder thread. Dropping it stops decoding: the thread's next
/// send fails, it exits, and the ffmpeg process is killed.
struct Decoder {
    rx: Receiver<Msg>,
}

impl Decoder {
    fn spawn(engine: Arc<dyn Engine>, req: PreviewRequest, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::sync_channel(BUFFER);
        thread::spawn(move || {
            let mut source = match engine.open_preview(&req) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Msg::Error(e.to_string()));
                    ctx.request_repaint();
                    return;
                }
            };
            loop {
                let msg = match source.next_frame() {
                    Ok(Some(f)) => Msg::Frame(f),
                    Ok(None) => Msg::End,
                    Err(e) => Msg::Error(e.to_string()),
                };
                let last = !matches!(msg, Msg::Frame(_));
                if tx.send(msg).is_err() || last {
                    break;
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        });
        Self { rx }
    }
}

pub struct Player {
    engine: Arc<dyn Engine>,
    video: PathBuf,
    subtitle: Option<PathBuf>,
    show_subtitles: bool,
    duration: Option<Duration>,
    fps: f64,

    texture: Option<TextureHandle>,
    /// Timestamp of the frame on screen (or where we'll seek to).
    position: Duration,
    playing: bool,
    decoder: Option<Decoder>,
    needs_decoder: bool,
    /// Wall-clock instant matching `clock_pts`, set when the first frame arrives
    /// so ffmpeg's startup delay doesn't make us skip frames.
    clock: Option<(Instant, Duration)>,
    /// Next frame, decoded but not due yet.
    queued: Option<Frame>,
    /// Slider value while the user is dragging.
    scrub: Option<f64>,
    error: Option<String>,
}

impl Player {
    pub fn new(
        engine: Arc<dyn Engine>,
        video: PathBuf,
        duration: Option<Duration>,
        source_fps: Option<f64>,
    ) -> Self {
        Self {
            engine,
            video,
            subtitle: None,
            show_subtitles: true,
            duration,
            fps: PreviewRequest::preview_fps(source_fps),
            texture: None,
            position: Duration::ZERO,
            // Developer aid (see app.rs `dev`): start playing immediately.
            playing: std::env::var_os("CUTFRAME_AUTOPLAY").is_some(),
            decoder: None,
            needs_decoder: true,
            clock: None,
            queued: None,
            scrub: None,
            error: None,
        }
    }

    pub fn set_subtitle(&mut self, path: Option<PathBuf>) {
        self.subtitle = path;
        self.restart();
    }

    /// Stop playback (e.g. while an export is running).
    pub fn pause(&mut self) {
        if self.playing {
            self.playing = false;
            self.restart();
        }
    }

    fn seek(&mut self, to: Duration) {
        self.position = self.duration.map_or(to, |d| to.min(d));
        self.restart();
    }

    /// Throw away the current decoder and start a new one at `position`.
    fn restart(&mut self) {
        self.decoder = None;
        self.queued = None;
        self.clock = None;
        self.error = None;
        self.needs_decoder = true;
    }

    fn ensure_decoder(&mut self, ctx: &egui::Context) {
        if !self.needs_decoder {
            return;
        }
        self.needs_decoder = false;
        let mut req = PreviewRequest::new(self.video.clone());
        req.start = self.position;
        req.fps = self.fps;
        req.single_frame = !self.playing;
        req.subtitle = self.subtitle.clone().filter(|_| self.show_subtitles);
        self.decoder = Some(Decoder::spawn(self.engine.clone(), req, ctx.clone()));
    }

    fn show_frame(&mut self, ctx: &egui::Context, frame: Frame) {
        let size = [frame.width as usize, frame.height as usize];
        let image = ColorImage::from_rgb(size, &frame.rgb);
        match &mut self.texture {
            Some(t) => t.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR)),
        }
        self.position = frame.pts;
    }

    /// Pull due frames from the decoder and schedule the next repaint.
    fn pump(&mut self, ctx: &egui::Context) {
        self.ensure_decoder(ctx);
        loop {
            let frame = match self.queued.take() {
                Some(f) => f,
                None => {
                    let Some(dec) = &self.decoder else { return };
                    match dec.rx.try_recv() {
                        Ok(Msg::Frame(f)) => f,
                        Ok(Msg::End) | Err(TryRecvError::Disconnected) => {
                            self.decoder = None;
                            self.playing = false;
                            return;
                        }
                        Ok(Msg::Error(e)) => {
                            self.error = Some(e);
                            self.decoder = None;
                            self.playing = false;
                            return;
                        }
                        Err(TryRecvError::Empty) => return,
                    }
                }
            };

            if !self.playing {
                self.show_frame(ctx, frame);
                continue;
            }
            let (start, start_pts) = *self.clock.get_or_insert((Instant::now(), frame.pts));
            let now = start_pts + start.elapsed();
            if frame.pts <= now {
                // Due (or late): show it, then see if a newer one is also due.
                self.show_frame(ctx, frame);
            } else {
                ctx.request_repaint_after(frame.pts - now);
                self.queued = Some(frame);
                return;
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);

        // Picture
        let width = ui.available_width();
        match &self.texture {
            Some(tex) => {
                let [w, h] = tex.size().map(|v| v as f32);
                let scale = (width / w).min(MAX_HEIGHT / h);
                let size = egui::vec2(w * scale, h * scale);
                ui.vertical_centered(|ui| {
                    ui.add(egui::Image::from_texture((tex.id(), size)).fit_to_exact_size(size));
                });
            }
            None => {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, 180.0_f32.min(width * 0.56)), egui::Sense::hover());
                ui.painter().rect_filled(rect, 4.0, Color32::from_gray(20));
                let msg = if self.error.is_some() { "Preview unavailable" } else { "Loading…" };
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    msg,
                    egui::FontId::proportional(14.0),
                    Color32::GRAY,
                );
            }
        }

        // Transport controls
        ui.horizontal(|ui| {
            let icon = if self.playing { "⏸" } else { "▶" };
            if ui.button(RichText::new(icon).size(16.0)).on_hover_text("Play / pause (Space)").clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Space)) && !ctx.egui_wants_keyboard_input()
            {
                self.toggle_play();
            }

            let total = self.duration.unwrap_or_default();
            ui.monospace(format!(
                "{} / {}",
                format_duration(self.scrub.map_or(self.position, Duration::from_secs_f64)),
                format_duration(total)
            ));

            if !total.is_zero() {
                let mut t = self.scrub.unwrap_or(self.position.as_secs_f64());
                ui.spacing_mut().slider_width = (ui.available_width() - 130.0).max(60.0);
                let slider = ui.add(
                    egui::Slider::new(&mut t, 0.0..=total.as_secs_f64()).show_value(false),
                );
                if slider.dragged() {
                    self.scrub = Some(t);
                }
                if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                    self.scrub = None;
                    self.seek(Duration::from_secs_f64(t));
                }
            }

            if self.subtitle.is_some() && ui.checkbox(&mut self.show_subtitles, "Subtitles").changed() {
                self.restart();
            }
        });

        if let Some(e) = &self.error {
            ui.small(RichText::new(last_line(e)).color(Color32::LIGHT_RED)).on_hover_text(e);
        }
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing;
        if self.playing && self.duration.is_some_and(|d| self.position + Duration::from_millis(100) >= d) {
            // At the end: play again from the start.
            self.position = Duration::ZERO;
        }
        self.restart();
    }
}

/// ffmpeg puts the useful part of an error at the end of its log.
fn last_line(s: &str) -> &str {
    s.lines().rfind(|l| !l.trim().is_empty()).unwrap_or(s)
}
