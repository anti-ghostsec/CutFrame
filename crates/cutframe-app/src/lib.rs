//! CutFrame GUI. The app lives in a library so the desktop binary and a future
//! Android `cdylib` (`android_main`) can share it.

mod app;
pub mod player;
mod tools;

pub use app::CutFrameApp;

pub const APP_NAME: &str = "CutFrame";

#[cfg(not(target_os = "android"))]
pub fn run_desktop() -> eframe::Result {
    // `cutframe [--trim] video.mp4 [subs.srt]` (also how "Open with…" passes files).
    let mut start_tool = None;
    let mut files = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--trim") => start_tool = Some("Trim / Cut"),
            Some("--subtitles") => start_tool = Some("Embed subtitles"),
            _ => files.push(std::path::PathBuf::from(arg)),
        }
    }
    files.retain(|p| p.is_file());
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([820.0, 900.0])
            .with_min_inner_size([480.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(CutFrameApp::new(cc, start_tool, files)))))
}
