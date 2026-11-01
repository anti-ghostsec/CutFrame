//! CutFrame GUI. The app lives in a library so the desktop binary and a future
//! Android `cdylib` (`android_main`) can share it.

mod app;
pub mod player;
mod tools;

pub use app::CutFrameApp;

pub const APP_NAME: &str = "CutFrame";

#[cfg(not(target_os = "android"))]
pub fn run_desktop() -> eframe::Result {
    // `cutframe video.mp4 subs.srt` (also how "Open with…" passes files).
    let files: Vec<std::path::PathBuf> =
        std::env::args_os().skip(1).map(Into::into).filter(|p: &std::path::PathBuf| p.is_file()).collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([720.0, 620.0])
            .with_min_inner_size([480.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(CutFrameApp::new(cc, files)))))
}
