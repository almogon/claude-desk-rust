#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use claude_deck::ui::{DeckApp, window_icon};

fn main() -> eframe::Result {
    claude_deck::init_tracing();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Claude Deck")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Claude Deck",
        options,
        Box::new(|cc| Ok(Box::new(DeckApp::new(cc)))),
    )
}
