//! Pure rendering: `&State → Element`. No I/O, no mutation.

pub mod chat;
pub mod login;
pub mod markup;
pub mod members;
pub mod modal;
pub mod search;
pub mod settings;
pub mod sidebar;

use chrono::{DateTime, Datelike, Local, Utc};
use iced::widget::{button, center, column, container, row, text};
use iced::{Element, Fill, Length};
use plume_lite_protocol::http::ClientRelease;
use plume_lite_protocol::model::User;

use crate::app::{Chat, Msg, UpdateProgress};
use crate::theme;

pub fn splash<'a>() -> Element<'a, Msg> {
    container(center(
        column![text("Plume Lite").size(32).font(theme::BOLD), text("Connexion…").color(theme::c().muted)]
            .spacing(8)
            .align_x(iced::Center),
    ))
    .style(theme::fill(theme::c().rail))
    .into()
}

/// Rail | channels | messages | members, with an optional modal on top.
pub fn chat_screen(chat: &Chat) -> Element<'_, Msg> {
    let layout = row![
        sidebar::rail(chat),
        sidebar::channels(chat),
        container(chat::view(chat)).width(Fill).height(Fill).style(theme::fill(theme::c().chat)),
        match &chat.search {
            Some(panel) => search::view(chat, panel),
            None => members::view(chat),
        },
    ]
    .height(Fill);
    let screen: Element<'_, Msg> = match &chat.update {
        Some(release) => column![update_banner(chat, release), layout].into(),
        None => layout.into(),
    };
    match (&chat.settings, &chat.modal) {
        (Some(settings), _) => settings::overlay(screen, chat, settings),
        (None, Some(modal)) => modal::overlay(screen, chat, modal),
        (None, None) => screen,
    }
}

/// "Plume Lite 0.5.1 est disponible. [Télécharger]", then the download and its check.
fn update_banner<'a>(chat: &'a Chat, release: &'a ClientRelease) -> Element<'a, Msg> {
    let action = |label: &'a str, msg: Option<Msg>| {
        button(text(label).font(theme::SEMIBOLD)).padding([4, 12]).style(theme::primary).on_press_maybe(msg)
    };
    let (message, button) = match &chat.update_progress {
        UpdateProgress::Idle => (
            format!("Plume Lite {} est disponible.", release.version),
            action("Télécharger", Some(Msg::DownloadUpdate)),
        ),
        UpdateProgress::Downloading => {
            (format!("Téléchargement et vérification de Plume Lite {}…", release.version), action("Patientez…", None))
        }
        UpdateProgress::Ready(path) => (
            format!(
                "Plume Lite {} est téléchargé et sa signature vérifiée : {}. Quittez celui-ci, puis lancez le nouveau.",
                release.version,
                path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default()
            ),
            action("Ouvrir le dossier", Some(Msg::OpenUpdateFolder)),
        ),
    };
    container(row![text(message).font(theme::SEMIBOLD), button].spacing(12).align_y(iced::Center))
        .center_x(Fill)
        .padding(6)
        .style(theme::banner)
        .into()
}

/// Coloured disc with the user's initial (no image pipeline needed).
pub fn avatar<'a>(user: &User, size: f32) -> Element<'a, Msg> {
    let initial = user.username.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    container(text(initial).size(size * 0.45).font(theme::SEMIBOLD).color(theme::c().on_fill))
        .center(Length::Fixed(size))
        .style(theme::rounded_fill(theme::user_color(user.id), size / 2.0))
        .into()
}

/// "Rust France" → "RF", "général" → "G".
pub fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|word| word.chars().next()).take(2).flat_map(char::to_uppercase).collect()
}

const MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

/// "Aujourd'hui à 14:32", "Hier à 09:05", "03/10/2026 18:00".
pub fn message_time(at: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    let today = Local::now().date_naive();
    let time = local.format("%H:%M");
    match (today - local.date_naive()).num_days() {
        0 => format!("Aujourd'hui à {time}"),
        1 => format!("Hier à {time}"),
        _ => local.format("%d/%m/%Y %H:%M").to_string(),
    }
}

/// "7 octobre 2026" for day separators.
pub fn day_label(at: DateTime<Utc>) -> String {
    let local = at.with_timezone(&Local);
    format!("{} {} {}", local.day(), MONTHS[local.month0() as usize], local.year())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_are_short_and_uppercase() {
        assert_eq!(initials("Rust France"), "RF");
        assert_eq!(initials("général"), "G");
        assert_eq!(initials("les amis du code"), "LA");
        assert_eq!(initials(""), "");
    }
}
