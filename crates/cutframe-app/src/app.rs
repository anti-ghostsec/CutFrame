use std::sync::Arc;

use cutframe_core::{Engine, FfmpegCli};
use eframe::egui;

use crate::tools::{subtitles::SubtitleTool, Tool};

pub struct CutFrameApp {
    engine_name: String,
    tools: Vec<Box<dyn Tool>>,
    active: usize,
}

impl CutFrameApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let engine: Arc<dyn Engine> = Arc::new(FfmpegCli::locate());
        Self {
            engine_name: engine.name(),
            tools: vec![Box::new(SubtitleTool::new(engine))],
            active: 0,
        }
    }
}

impl eframe::App for CutFrameApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
