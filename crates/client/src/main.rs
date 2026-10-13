//! Plume Lite — native desktop client (Rust + iced, no web engine).

// Release builds on Windows open no console window next to the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod app;
mod compose;
mod gateway;
mod notify;
mod session;
mod sound;
mod store;
mod theme;
mod tray;
mod update;
mod view;
mod voice;

use iced::{Font, Size, window};

/// Bundled so that messages look the same everywhere (italics and code included): the
/// default font of iced, Open Sans, is rarely installed. SIL Open Font License, see
/// `fonts/OFL.txt`.
const FONTS: [&[u8]; 5] = [
    include_bytes!("../fonts/NotoSans-Regular.ttf"),
    include_bytes!("../fonts/NotoSans-Italic.ttf"),
    include_bytes!("../fonts/NotoSans-Bold.ttf"),
    include_bytes!("../fonts/NotoSans-BoldItalic.ttf"),
    // The family of `Font::MONOSPACE` for iced's text engine.
    include_bytes!("../fonts/NotoSansMono-Regular.ttf"),
];

fn main() -> iced::Result {
    // One explicit TLS crypto backend for both HTTPS (reqwest) and WSS (tungstenite).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let application = iced::application(app::App::boot, app::App::update, app::App::view);
    FONTS
        .into_iter()
        .fold(application, |application, font| application.font(font))
        .default_font(Font::with_name("Noto Sans"))
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(window::Settings {
            size: Size::new(1200.0, 760.0),
            min_size: Some(Size::new(860.0, 520.0)),
            // Closing hides the window when there is a notification area (see `tray.rs`).
            exit_on_close_request: false,
            ..window::Settings::default()
        })
        .run()
}
