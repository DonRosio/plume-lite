use iced::widget::{button, center, column, container, mouse_area, opaque, row, space, stack, text, text_input};
use iced::{Element, Fill};

use crate::app::{Chat, MODAL_INPUT, Modal, ModalKind, Msg};
use crate::theme;

/// Draws `modal` above `base`; clicking the backdrop closes it.
pub fn overlay<'a>(base: Element<'a, Msg>, chat: &'a Chat, modal: &'a Modal) -> Element<'a, Msg> {
    let card = container(content(chat, modal)).width(460).padding(24).style(theme::card);
    stack![base, opaque(mouse_area(center(opaque(card)).style(theme::backdrop)).on_press(Msg::CloseModal)),].into()
}

fn content<'a>(chat: &'a Chat, modal: &'a Modal) -> Element<'a, Msg> {
    let guild_name = |id| chat.store.guild(id).map_or("ce serveur", |g| g.name.as_str());
    let channel_name = |id| chat.store.channel(id).map_or("ce salon", |c| c.name.as_str());

    let (title, description, input, confirm, danger): (String, String, Option<&str>, &str, bool) = match &modal.kind {
        ModalKind::CreateGuild => (
            "Créer un serveur".into(),
            "Votre serveur est l'endroit où vous retrouvez vos amis. Vous pourrez inviter du monde ensuite.".into(),
            Some("Nom du serveur"),
            "Créer",
            false,
        ),
        ModalKind::JoinGuild => (
            "Rejoindre un serveur".into(),
            "Collez le code ou le lien d'invitation que l'on vous a envoyé.".into(),
            Some("AbCd1234 ou https://…/invite/AbCd1234"),
            "Rejoindre",
            false,
        ),
        ModalKind::CreateChannel(_) => (
            "Créer un salon textuel".into(),
            "Les noms sont normalisés : « Mon Salon » devient « mon-salon ».".into(),
            Some("nouveau-salon"),
            "Créer le salon",
            false,
        ),
        ModalKind::Invite { invite } => {
            return invite_content(invite.as_ref(), modal);
        }
        ModalKind::LeaveGuild(id) => (
            format!("Quitter « {} »", guild_name(*id)),
            "Vous ne pourrez revenir qu'avec une nouvelle invitation.".into(),
            None,
            "Quitter le serveur",
            true,
        ),
        ModalKind::DeleteGuild(id) => (
            format!("Supprimer « {} »", guild_name(*id)),
            "Tous les salons et messages seront définitivement supprimés. Cette action est irréversible.".into(),
            None,
            "Supprimer le serveur",
            true,
        ),
        ModalKind::DeleteChannel(id) => (
            format!("Supprimer #{}", channel_name(*id)),
            "Tous les messages de ce salon seront définitivement supprimés.".into(),
            None,
            "Supprimer le salon",
            true,
        ),
    };

    let field = input.map(|placeholder| {
        text_input(placeholder, &modal.input)
            .id(MODAL_INPUT)
            .on_input(Msg::ModalInput)
            .on_submit(Msg::SubmitModal)
            .padding(10)
            .style(theme::field)
    });
    let error = modal.error.as_deref().map(|e| text(e).size(14).color(theme::c().red));
    let confirm_button = button(text(if modal.busy { "Patientez…" } else { confirm }).font(theme::SEMIBOLD))
        .padding([10, 16])
        .style(if danger { theme::danger } else { theme::primary })
        .on_press_maybe((!modal.busy).then_some(Msg::SubmitModal));

    column![
        text(title).size(20).font(theme::BOLD).color(theme::c().bright),
        text(description).color(theme::c().muted),
        field,
        error,
        row![
            space::horizontal(),
            button(text("Annuler")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal),
            confirm_button
        ]
        .spacing(8),
    ]
    .spacing(16)
    .into()
}

fn invite_content<'a>(invite: Option<&'a plume_lite_protocol::model::Invite>, modal: &'a Modal) -> Element<'a, Msg> {
    let body: Element<'a, Msg> = match (invite, &modal.error) {
        (_, Some(error)) => text(error).color(theme::c().red).into(),
        (None, None) => text("Génération du lien…").color(theme::c().muted).into(),
        (Some(invite), None) => column![
            text("Envoyez ce code à vos amis (valable 7 jours) :").color(theme::c().muted),
            container(
                row![
                    text(&invite.code).size(18).font(theme::BOLD).color(theme::c().bright).width(Fill),
                    button(text("Copier").font(theme::SEMIBOLD))
                        .padding([8, 14])
                        .style(theme::primary)
                        .on_press(Msg::Copy(invite.code.clone())),
                ]
                .align_y(iced::Center),
            )
            .padding([8, 12])
            .style(theme::rounded_fill(theme::c().rail, 6.0)),
            text(format!("{} membre(s) dans {}", invite.member_count, invite.guild.name))
                .size(12)
                .color(theme::c().muted),
        ]
        .spacing(12)
        .into(),
    };
    column![
        text("Inviter des amis").size(20).font(theme::BOLD).color(theme::c().bright),
        body,
        row![
            space::horizontal(),
            button(text("Terminé")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal)
        ],
    ]
    .spacing(16)
    .into()
}
