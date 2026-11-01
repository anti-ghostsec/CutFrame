//! Pluggable tools. Each tool is one screen of the app; the subtitle embedder
//! is the first, and future editor features (trim, merge, timeline…) get added
//! here as new tools.

pub mod common;
pub mod subtitles;
pub mod trim;

pub trait Tool {
    fn name(&self) -> &'static str;
    fn ui(&mut self, ui: &mut eframe::egui::Ui);
    /// Open a file passed on the command line. Returns false if not handled.
    fn open(&mut self, _path: &std::path::Path) -> bool {
        false
    }
    /// True while a background job is running (used to block closing/switching).
    fn is_busy(&self) -> bool {
        false
    }
}
