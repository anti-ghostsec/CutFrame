use std::path::PathBuf;
use std::sync::Arc;

use cutframe_core::{Engine, FfmpegCli};
use eframe::egui;

use crate::tools::{subtitles::SubtitleTool, trim::TrimTool, Tool};

pub struct CutFrameApp {
    engine_name: String,
    tools: Vec<Box<dyn Tool>>,
    active: usize,
    dev_screenshot: Option<dev::Screenshot>,
}

impl CutFrameApp {
    /// `start_tool` picks the initial tab by name; `files` are opened in it
    /// (or in the first tool that accepts them).
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        start_tool: Option<&str>,
        files: Vec<PathBuf>,
    ) -> Self {
        let engine: Arc<dyn Engine> = Arc::new(FfmpegCli::locate());
        let mut app = Self {
            engine_name: engine.name(),
            tools: vec![
                Box::new(SubtitleTool::new(engine.clone())),
                Box::new(TrimTool::new(engine)),
            ],
            active: 0,
            dev_screenshot: dev::Screenshot::from_env(),
        };
        if let Some(i) = app.tools.iter().position(|t| Some(t.name()) == start_tool) {
            app.active = i;
        }
        for f in files {
            let active = app.active;
            if !app.tools[active].open(&f) {
                app.tools.iter_mut().any(|t| t.open(&f));
            }
        }
        app
    }
}

impl eframe::App for CutFrameApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(shot) = &mut self.dev_screenshot {
            shot.tick(ui.ctx());
        }
        let busy = self.tools[self.active].is_busy();

        egui::Panel::top("tool_tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(crate::APP_NAME);
                ui.separator();
                for (i, tool) in self.tools.iter().enumerate() {
                    let btn = ui.add_enabled(
                        !busy || i == self.active,
                        egui::Button::selectable(i == self.active, tool.name()),
                    );
                    if btn.clicked() {
                        self.active = i;
                    }
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.small(format!("Engine: {}", self.engine_name));
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.tools[self.active].ui(ui));
        });
    }
}

/// Developer aid for checking the UI without a screen grabber:
/// `CUTFRAME_SCREENSHOT=out.ppm CUTFRAME_SCREENSHOT_AFTER=2.5 cutframe …`
/// saves the window after the given number of seconds. Add `CUTFRAME_AUTOPLAY=1`
/// to start the preview playing.
mod dev {
    use std::time::{Duration, Instant};

    use eframe::egui;

    pub struct Screenshot {
        path: String,
        at: Instant,
        requested: bool,
    }

    impl Screenshot {
        pub fn from_env() -> Option<Self> {
            let path = std::env::var("CUTFRAME_SCREENSHOT").ok()?;
            let after = std::env::var("CUTFRAME_SCREENSHOT_AFTER")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(2.0);
            Some(Self { path, at: Instant::now() + Duration::from_secs_f64(after), requested: false })
        }

        pub fn tick(&mut self, ctx: &egui::Context) {
            if self.requested {
                return;
            }
            let now = Instant::now();
            if now < self.at {
                ctx.request_repaint_after(self.at - now);
                return;
            }
            self.requested = true;
            let path = self.path.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(
                move |img| {
                    let [w, h] = img.size;
                    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
                    out.extend(img.pixels.iter().flat_map(|c| [c.r(), c.g(), c.b()]));
                    let _ = std::fs::write(&path, out);
                },
            )));
        }
    }
}
