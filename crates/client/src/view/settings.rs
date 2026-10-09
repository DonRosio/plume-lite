//! Settings window: change the password, see and log out the other devices.

use iced::widget::{
    button, center, column, container, mouse_area, opaque, row, rule, scrollable, space, stack, text, text_input,
};
use iced::{Center, Element, Fill};
use plume_lite_protocol::model::SessionInfo;

use crate::app::{Msg, Settings};
use crate::theme;
use crate::view::message_time;

/// Draws the settings above `base`; clicking the backdrop closes them.
pub fn overlay<'a>(base: Element<'a, Msg>, settings: &'a Settings) -> Element<'a, Msg> {
    let card =
        container(scrollable(content(settings)).spacing(8)).width(520).max_height(640).padding(24).style(theme::card);
    stack![base, opaque(mouse_area(center(opaque(card)).style(theme::backdrop)).on_press(Msg::Escape))].into()
}

fn content(settings: &Settings) -> Element<'_, Msg> {
    let c = theme::c();
    let heading = |label: &'static str| text(label).size(16).font(theme::BOLD).color(c.bright);
    let label = |label: &'static str| text(label).size(12).font(theme::BOLD).color(c.muted);
    let field = |placeholder: &'static str, value: &str, on_input: fn(String) -> Msg| {
        text_input(placeholder, value)
            .secure(true)
            .on_input(on_input)
            .on_submit(Msg::SubmitPassword)
            .padding(10)
            .style(theme::field)
    };

    let status = match (&settings.error, settings.saved) {
        (Some(error), _) => Some(text(error).size(14).color(c.red)),
        (None, true) => {
            Some(text("Mot de passe changé. Vos autres appareils ont été déconnectés.").size(14).color(c.green))
        }
        (None, false) => None,
    };
    let submit =
        button(text(if settings.busy { "Patientez…" } else { "Changer le mot de passe" }).font(theme::SEMIBOLD))
            .padding([10, 16])
            .style(theme::primary)
            .on_press_maybe((!settings.busy).then_some(Msg::SubmitPassword));
    let password = column![
        heading("Mot de passe"),
        column![label("MOT DE PASSE ACTUEL"), field("••••••••", &settings.current, Msg::SettingsCurrent)].spacing(8),
        column![label("NOUVEAU MOT DE PASSE"), field("8 caractères minimum", &settings.new, Msg::SettingsNew)]
            .spacing(8),
        column![label("CONFIRMATION"), field("••••••••", &settings.confirm, Msg::SettingsConfirm)].spacing(8),
    ]
    .push(status)
    .push(row![space::horizontal(), submit])
    .spacing(12);

    let devices: Element<'_, Msg> = match &settings.sessions {
        None => text("Chargement…").color(c.muted).into(),
        Some(Err(error)) => text(error).color(c.red).into(),
        Some(Ok(sessions)) => column(sessions.iter().map(device)).spacing(8).into(),
    };

    column![
        row![
            text("Paramètres").size(20).font(theme::BOLD).color(c.bright).width(Fill),
            button(text("Fermer")).padding([6, 12]).style(theme::ghost).on_press(Msg::Escape),
        ]
        .align_y(Center),
        password,
        rule::horizontal(1),
        column![
            heading("Appareils connectés"),
            text("Déconnectez un appareil que vous n'utilisez plus ou que vous ne reconnaissez pas.")
                .size(13)
                .color(c.muted),
            devices,
        ]
        .spacing(12),
        text(format!("Plume Lite {}", env!("CARGO_PKG_VERSION"))).size(12).color(c.muted),
    ]
    .spacing(20)
    .into()
}

fn device(session: &SessionInfo) -> Element<'_, Msg> {
    let c = theme::c();
    let action: Element<'_, Msg> = if session.current {
        text("Cet appareil").size(13).font(theme::SEMIBOLD).color(c.green).into()
    } else {
        button(text("Déconnecter").size(13))
            .padding([6, 12])
            .style(theme::danger)
            .on_press(Msg::RevokeSession(session.id))
            .into()
    };
    let seen = message_time(session.last_seen_at);
    let mut seen_chars = seen.chars();
    let seen = seen_chars.next().map(|first| first.to_lowercase().chain(seen_chars).collect()).unwrap_or(seen);
    container(
        row![
            column![
                text(device_label(&session.device)).font(theme::SEMIBOLD).color(c.bright),
                text(format!("Dernière activité : {seen}")).size(12).color(c.muted),
            ]
            .spacing(2)
            .width(Fill),
            action,
        ]
        .spacing(12)
        .align_y(Center),
    )
    .padding([8, 12])
    .style(theme::rounded_fill(c.sidebar, 6.0))
    .into()
}

/// `plume-lite-desktop/0.3.0 (windows)` → "Plume Lite 0.3.0 · Windows".
pub fn device_label(device: &str) -> String {
    let Some(rest) = device.strip_prefix("plume-lite-desktop/") else {
        return if device.is_empty() { "Appareil inconnu".to_owned() } else { device.to_owned() };
    };
    let (version, os) = rest.split_once(" (").map_or((rest, ""), |(version, os)| (version, os.trim_end_matches(')')));
    let os = match os {
        "windows" => "Windows",
        "linux" => "Linux",
        "macos" => "macOS",
        other => other,
    };
    if os.is_empty() { format!("Plume Lite {version}") } else { format!("Plume Lite {version} · {os}") }
}

#[cfg(test)]
mod tests {
    use super::device_label;

    #[test]
    fn device_labels() {
        assert_eq!(device_label("plume-lite-desktop/0.3.0 (windows)"), "Plume Lite 0.3.0 · Windows");
        assert_eq!(device_label("plume-lite-desktop/0.2.0"), "Plume Lite 0.2.0");
        assert_eq!(device_label(""), "Appareil inconnu");
        assert_eq!(device_label("curl/8.5.0"), "curl/8.5.0");
    }
}
