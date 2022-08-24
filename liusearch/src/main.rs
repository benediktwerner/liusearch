#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rfd::{MessageButtons, MessageDialog, MessageLevel};

mod api;
mod app;
mod model;

fn setup_panic_hook() {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("Panic: {}", info);
        default_panic_hook(info);
        MessageDialog::new()
            .set_title("Panic")
            .set_buttons(MessageButtons::Ok)
            .set_description(&format!("Panic:\n\n{}", info))
            .set_level(MessageLevel::Error)
            .show();
    }));
}

fn main() {
    setup_panic_hook();

    let app = app::App::default();
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(Box::new(app), native_options);
}
