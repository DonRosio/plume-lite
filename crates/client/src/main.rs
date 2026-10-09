//! Plume Lite — native desktop client (Rust + iced, no web engine).

// Release builds on Windows open no console window next to the app.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod app;
mod gateway;
mod session;
mod store;
mod theme;
mod view;

use iced::{Size, window};

fn main() -> iced::Result {
    // One explicit TLS crypto backend for both HTTPS (reqwest) and WSS (tungstenite).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    iced::application(app::App::boot, app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(window::Settings {
            size: Size::new(1200.0, 760.0),
            min_size: Some(Size::new(860.0, 520.0)),
            ..window::Settings::default()
        })
        .run()
}
