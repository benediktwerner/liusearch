#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::cast_precision_loss)]

use app::App;
use rfd::{MessageButtons, MessageDialog, MessageLevel};

mod api;
mod app;
mod model;

fn setup_panic_hook() {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("Panic: {info}");
        default_panic_hook(info);
        MessageDialog::new()
            .set_title("Panic")
            .set_buttons(MessageButtons::Ok)
            .set_description(&format!("Panic:\n\n{info}"))
            .set_level(MessageLevel::Error)
            .show();
    }));
}

fn main() {
    setup_panic_hook();

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Lichess User Search",
        native_options,
        Box::new(|cc| Box::new(App::new(cc))),
    )
    .unwrap();
}
