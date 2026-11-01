use std::path::PathBuf;
use std::sync::Arc;

use cutframe_core::{Engine, FfmpegCli};
use eframe::egui::{self, Color32, RichText};

use crate::tools::common::dropped_files;
use crate::tools::{subtitles::SubtitleTool, trim::TrimTool, Tool};

/// Features planned for the full editor, shown greyed out on Home.
const COMING_SOON: &[(&str, &str, &str)] = &[
    ("🔗", "Join videos", "Put several clips one after another."),
    ("🎞", "Timeline editor", "Arrange clips, audio and titles on tracks."),
    ("📐", "Crop & resize", "Change the frame size or aspect ratio."),
];

pub struct CutFrameApp {
    engine_name: String,
    tools: Vec<Box<dyn Tool>>,
    /// `None` = Home screen.
    active: Option<usize>,
    /// Files opened from Home, so the user knows they're loaded.
    opened: Vec<PathBuf>,
    dev_screenshot: Option<dev::Screenshot>,
}

impl CutFrameApp {
    /// `start_tool` picks the initial tool by name (otherwise Home).
    /// `files` are opened in every tool that can use them.
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        start_tool: Option<&str>,
        files: Vec<PathBuf>,
    ) -> Self {
        let engine: Arc<dyn Engine> = Arc::new(FfmpegCli::locate());
        let mut app = Self {
            engine_name: engine.name(),
            tools: vec![
                Box::new(TrimTool::new(engine.clone())),
                Box::new(SubtitleTool::new(engine)),
            ],
            active: None,
            opened: Vec::new(),
            dev_screenshot: dev::Screenshot::from_env(),
        };
        app.active = app.tools.iter().position(|t| Some(t.name()) == start_tool);
        app.open_everywhere(files);
        app
    }

    fn open_everywhere(&mut self, files: Vec<PathBuf>) {
        for f in files {
            let mut used = false;
            for tool in &mut self.tools {
                used |= tool.open(&f);
            }
            if used && !self.opened.contains(&f) {
                self.opened.push(f);
            }
        }
    }

    fn switch_to(&mut self, target: Option<usize>) {
        if target == self.active {
            return;
        }
        if let Some(i) = self.active {
            self.tools[i].on_hide();
        }
        self.active = target;
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| self.nav(ui));
    }

    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.heading(crate::APP_NAME);
        ui.add_space(8.0);

        let mut target = self.active;
        let nav = |ui: &mut egui::Ui, selected: bool, icon: &str, label: &str, busy: bool| {
            let text = if busy { format!("{icon}  {label}   ⏳") } else { format!("{icon}  {label}") };
            let resp = ui.add_sized(
                [ui.available_width(), 30.0],
                egui::Button::selectable(selected, text),
            );
            let resp = if busy { resp.on_hover_text("Working…") } else { resp };
            resp.clicked()
        };

        if nav(ui, self.active.is_none(), "🏠", "Home", false) {
            target = None;
        }
        ui.add_space(6.0);
        ui.label(RichText::new("TOOLS").small().weak());
        for (i, tool) in self.tools.iter().enumerate() {
            if nav(ui, self.active == Some(i), tool.icon(), tool.name(), tool.is_busy()) {
                target = Some(i);
            }
        }
        self.switch_to(target);
    }

    fn home(&mut self, ui: &mut egui::Ui) {
        // Files dropped on Home go to every tool that can use them.
        let dropped = dropped_files(ui.ctx());
        if !dropped.is_empty() {
            self.open_everywhere(dropped);
        }

        ui.add_space(12.0);
        ui.heading("What do you want to do?");
        if self.opened.is_empty() {
            ui.label(RichText::new("Pick a tool, or drop a video here to open it in all of them.").weak());
        } else {
            let names: Vec<String> = self
                .opened
                .iter()
                .map(|p| p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().into_owned())
                .collect();
            ui.label(format!("Opened: {}. Pick a tool to continue.", names.join(", ")));
        }
        ui.add_space(12.0);

        const CARD: egui::Vec2 = egui::vec2(250.0, 96.0);
        let spacing = 12.0;
        let cols = ((ui.available_width() + spacing) / (CARD.x + spacing)).floor().max(1.0) as usize;

        let mut open = None;
        let cards: Vec<(&str, &str, &str, Option<usize>)> = self
            .tools
            .iter()
            .enumerate()
            .map(|(i, t)| (t.icon(), t.name(), t.description(), Some(i)))
            .chain(COMING_SOON.iter().map(|&(icon, name, desc)| (icon, name, desc, None)))
            .collect();

        egui::Grid::new("home_cards").spacing([spacing, spacing]).show(ui, |ui| {
            for (n, (icon, name, desc, tool)) in cards.into_iter().enumerate() {
                if card(ui, CARD, icon, name, desc, tool.is_some()) {
                    open = tool;
                }
                if (n + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });

        if open.is_some() {
            self.switch_to(open);
        }
    }
}

/// A clickable tool card. Returns true when clicked.
fn card(ui: &mut egui::Ui, size: egui::Vec2, icon: &str, name: &str, desc: &str, enabled: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = enabled && resp.hovered();
    let visuals = ui.visuals();
    let fill = if hovered { visuals.widgets.hovered.bg_fill } else { visuals.faint_bg_color };
    let stroke = if hovered { visuals.widgets.hovered.bg_stroke } else { visuals.widgets.noninteractive.bg_stroke };
    ui.painter().rect(rect, 8.0, fill, stroke, egui::StrokeKind::Inside);

    let mut content = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(12.0)));
    if !enabled {
        content.disable();
    }
    content.horizontal(|ui| {
        ui.label(RichText::new(icon).size(26.0));
        ui.vertical(|ui| {
            ui.label(RichText::new(name).strong().size(15.0));
            ui.add(egui::Label::new(RichText::new(desc).small()).wrap());
            if !enabled {
                ui.label(RichText::new("Coming soon").small().italics().color(Color32::GRAY));
            }
        });
    });
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
    } else {
        false
    }
}

impl eframe::App for CutFrameApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(shot) = &mut self.dev_screenshot {
            shot.tick(ui.ctx());
        }
        // Keep jobs in background tools progressing (and their spinners live).
        for tool in &mut self.tools {
            tool.poll();
        }
        if self.tools.iter().any(|t| t.is_busy()) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }

        egui::Panel::left("nav").resizable(false).exact_size(190.0).show(ui, |ui| self.sidebar(ui));

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.small(format!("Engine: {}", self.engine_name));
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.active {
                Some(i) => self.tools[i].ui(ui),
                None => self.home(ui),
            });
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
