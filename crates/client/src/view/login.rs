use iced::widget::{button, center, column, container, text, text_input};
use iced::{Element, Fill};

use crate::app::{LOGIN_USERNAME, LoginForm, Msg};
use crate::theme;

pub fn view(form: &LoginForm) -> Element<'_, Msg> {
    let label = |s: &'static str| text(s).size(12).font(theme::BOLD).color(theme::c().muted);
    let field =
        |placeholder: &str, value: &str| text_input(placeholder, value).padding(10).size(15).style(theme::field);

    let (title, subtitle, action, busy_label, toggle) = if form.register {
        (
            "Créer un compte",
            "Rejoignez vos communautés en quelques secondes.",
            "Créer mon compte",
            "Création…",
            "Vous avez déjà un compte ? Connectez-vous",
        )
    } else {
        (
            "Ça fait plaisir de vous revoir !",
            "Connectez-vous pour retrouver vos serveurs.",
            "Se connecter",
            "Connexion…",
            "Pas encore de compte ? Inscrivez-vous",
        )
    };

    let submit = button(text(if form.busy { busy_label } else { action }).font(theme::SEMIBOLD).width(Fill).center())
        .padding(12)
        .width(Fill)
        .style(theme::primary)
        .on_press_maybe((!form.busy).then_some(Msg::SubmitLogin));

    let error = form.error.as_deref().map(|e| text(e).size(14).color(theme::c().red));

    let card = column![
        column![
            text(title).size(24).font(theme::BOLD).color(theme::c().bright),
            text(subtitle).color(theme::c().muted)
        ]
        .spacing(6)
        .align_x(iced::Center)
        .width(Fill),
        column![
            label("SERVEUR"),
            field("http://127.0.0.1:8080", &form.server).on_input(Msg::LoginServer).on_submit(Msg::SubmitLogin)
        ]
        .spacing(8),
        column![
            label("NOM D'UTILISATEUR"),
            field("vincent", &form.username)
                .id(LOGIN_USERNAME)
                .on_input(Msg::LoginUsername)
                .on_submit(Msg::SubmitLogin)
        ]
        .spacing(8),
        column![
            label("MOT DE PASSE"),
            field("••••••••", &form.password).secure(true).on_input(Msg::LoginPassword).on_submit(Msg::SubmitLogin)
        ]
        .spacing(8),
    ]
    .push(error)
    .push(submit)
    .push(button(text(toggle).size(14)).padding(0).style(theme::link).on_press(Msg::ToggleRegister))
    .spacing(20)
    .padding(32)
    .width(480);

    container(center(container(card).style(theme::card))).style(theme::fill(theme::c().accent)).into()
}
