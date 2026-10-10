use iced::widget::{Row, button, center, column, container, mouse_area, opaque, row, space, stack, text, text_input};
use iced::{Element, Fill};

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::ChannelKind;

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
        ModalKind::CreateChannel(_, kind) => (
            if *kind == ChannelKind::Voice { "Créer un salon vocal" } else { "Créer un salon textuel" }.into(),
            "Les noms sont normalisés : « Mon Salon » devient « mon-salon ».".into(),
            Some("nouveau-salon"),
            "Créer le salon",
            false,
        ),
        ModalKind::Invite { invite } => {
            return invite_content(invite.as_ref(), modal);
        }
        ModalKind::VoiceMember { guild_id, user_id } => {
            return voice_member_content(chat, *guild_id, *user_id);
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

/// What the guild owner can do to someone in voice.
fn voice_member_content<'a>(chat: &'a Chat, guild_id: Snowflake, user_id: Snowflake) -> Element<'a, Msg> {
    let name = chat.store.user_name(guild_id, user_id).unwrap_or_else(|| "Ce membre".to_owned());
    let state = chat.store.voice_state(user_id).filter(|s| s.guild_id == guild_id);
    let server_muted = state.is_some_and(|s| s.server_mute);
    let mute = if server_muted {
        button(text("Lui rendre la parole").font(theme::SEMIBOLD))
            .padding([10, 16])
            .style(theme::primary)
            .on_press(Msg::ServerMute { guild_id, user_id, muted: false })
    } else {
        button(text("Couper son micro").font(theme::SEMIBOLD))
            .padding([10, 16])
            .style(theme::primary)
            .on_press(Msg::ServerMute { guild_id, user_id, muted: true })
    };
    let disconnect = button(text("Déconnecter du vocal").font(theme::SEMIBOLD))
        .padding([10, 16])
        .style(theme::danger)
        .on_press_maybe(state.is_some().then_some(Msg::DisconnectFromVoice { guild_id, user_id }));
    // The guild's other voice channels, to send them to.
    let current = state.and_then(|s| s.channel_id);
    let destinations: Vec<Element<'a, Msg>> = chat
        .store
        .channels_of(guild_id)
        .into_iter()
        .filter(|c| c.kind == ChannelKind::Voice && Some(c.id) != current)
        .map(|c| {
            button(text(format!("🔊 {}", c.name)).size(13))
                .padding([6, 10])
                .style(theme::ghost)
                .on_press(Msg::MoveToVoice { guild_id, user_id, channel_id: c.id })
                .into()
        })
        .collect();
    let move_to = (current.is_some() && !destinations.is_empty()).then(|| {
        column![
            text("Déplacer vers").size(13).font(theme::SEMIBOLD).color(theme::c().muted),
            Row::with_children(destinations).spacing(6).wrap(),
        ]
        .spacing(6)
    });
    column![
        text(name).size(20).font(theme::BOLD).color(theme::c().bright),
        text(if server_muted {
            "Son micro est coupé par vous : il entend les autres mais ne peut pas parler."
        } else {
            "Couper son micro l'empêche de parler dans les salons vocaux de ce serveur, jusqu'à ce que vous le rétablissiez."
        })
        .color(theme::c().muted),
        row![mute, disconnect].spacing(8),
        move_to,
        row![space::horizontal(), button(text("Fermer")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal)],
    ]
    .spacing(16)
    .into()
}
