mod api;
mod app;
mod model;

fn main() {
    let app = app::App::default();
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(Box::new(app), native_options);
}
