//! Pluggable tools. Each tool is one screen of the app, reached from the Home
//! screen or the sidebar. New editor features (join, crop, timeline…) are added
//! here as new tools.

pub mod common;
pub mod subtitles;
pub mod trim;

pub trait Tool {
    fn name(&self) -> &'static str;
    /// Single emoji/glyph shown in the sidebar and on the Home card.
    fn icon(&self) -> &'static str;
    /// One-line explanation for the Home card.
    fn description(&self) -> &'static str;
    fn ui(&mut self, ui: &mut eframe::egui::Ui);
    /// Open a file passed on the command line or dropped on Home.
    /// Returns false if this tool can't use it.
    fn open(&mut self, _path: &std::path::Path) -> bool {
        false
    }
    /// True while a background job is running.
    fn is_busy(&self) -> bool {
        false
    }
    /// Called every frame for every tool, visible or not, so background jobs
    /// can finish while the user is elsewhere.
    fn poll(&mut self) {}
    /// Called when the user switches away (e.g. to stop playback).
    fn on_hide(&mut self) {}
}
