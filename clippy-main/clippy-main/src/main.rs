#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clipcache;
mod config;
#[cfg(feature = "devtools")]
mod devtools;
mod editor;
mod i18n;
mod log;
mod mf;
mod record;
mod settings;
mod ui_kit;
mod screenshot;

use app::mode::Mode;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(feature = "devtools")]
    if let Some(code) = devtools::run(&args) {
        std::process::exit(code);
    }
    // Aplica el idioma elegido antes de abrir cualquier ventana (sus títulos ya van traducidos).
    config::load();
    match Mode::parse(&args) {
        Mode::ApplyUpdate(target) => std::process::exit(app::update::apply(&target)),
        Mode::Settings => settings::run(),
        Mode::Editor(file) => editor::run(file),
        Mode::Tray { background } => app::tray::run(background),
    }
}
