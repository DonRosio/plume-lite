use chrono::TimeDelta;
use iced::widget::{
    Column, button, center, column, container, mouse_area, rich_text, row, rule, scrollable, space, span, stack, text,
    text_input,
};
use iced::{Center, Element, Fill};
use plume_lite_protocol::model::Message;
use plume_lite_protocol::validate::MESSAGE_MAX;

use super::{avatar, day_label, message_time};
use crate::app::{COMPOSER, Chat, Connection, EDITOR, MESSAGES, ModalRequest, Msg};
use crate::store::{Pending, Timeline};
use crate::theme;

/// Consecutive messages of one author within this window are grouped under one header.
const GROUP_WINDOW: TimeDelta = TimeDelta::minutes(7);

pub fn view(chat: &Chat) -> Element<'_, Msg> {
    let Some(channel) = chat.current_channel() else {
        return empty_state(chat);
    };

    let mut header = row![
        text("#").size(22).color(theme::c().muted),
        text(&channel.name).size(16).font(theme::BOLD).color(theme::c().bright).width(Fill),
    ]
    .spacing(8)
    .align_y(Center)
    .padding([12, 16]);
    if chat.is_owner(channel.guild_id) {
        header = header.push(
            button(text("Supprimer le salon").size(13))
                .padding([4, 8])
                .style(theme::ghost)
                .on_press(Msg::OpenModal(ModalRequest::DeleteChannel(channel.id))),
        );
    }

    let banner = match &chat.connection {
        Connection::Reconnecting { reason, retry_in } => Some(
            container(
                text(format!("Connexion perdue ({reason}). Nouvelle tentative dans {} s…", retry_in.as_secs().max(1)))
                    .size(13),
            )
            .padding([6, 16])
            .width(Fill)
            .style(theme::banner),
        ),
        _ => None,
    };

    let toast = chat
        .toast
        .as_deref()
        .map(|message| container(text(message).size(14)).padding([8, 12]).width(Fill).style(theme::toast));

    let history: Element<'_, Msg> = match chat.store.timeline(channel.id) {
        Some(timeline) if timeline.loaded || !timeline.pending.is_empty() => messages(chat, &channel.name, timeline),
        _ => center(text("Chargement des messages…").color(theme::c().muted)).into(),
    };

    column![
        header,
        container(rule::horizontal(1)).width(Fill),
        banner,
        history,
        container(toast).padding([0, 16]),
        composer(chat, &channel.name),
        typing_line(chat, channel.id),
    ]
    .height(Fill)
    .into()
}

fn messages<'a>(chat: &'a Chat, channel_name: &str, timeline: &'a Timeline) -> Element<'a, Msg> {
    let mut list = Column::new().padding([16, 0]).width(Fill);

    list = if timeline.reached_start {
        list.push(
            column![
                text(format!("Bienvenue dans #{channel_name} !")).size(28).font(theme::BOLD).color(theme::c().bright),
                text(format!("C'est le tout début du salon #{channel_name}.")).color(theme::c().muted),
            ]
            .spacing(6)
            .padding([16, 16]),
        )
    } else {
        let label = if timeline.loading { "Chargement…" } else { "Charger les messages précédents" };
        list.push(
            container(
                button(text(label).size(13))
                    .style(theme::ghost)
                    .on_press_maybe((!timeline.loading).then_some(Msg::LoadOlder)),
            )
            .center_x(Fill)
            .padding(8),
        )
    };

    let mut previous: Option<&Message> = None;
    for message in &timeline.messages {
        let new_day = previous.is_none_or(|p| day_label(p.created_at) != day_label(message.created_at));
        if new_day {
            list = list.push(day_separator(message));
        }
        let grouped = !new_day
            && previous
                .is_some_and(|p| p.author.id == message.author.id && message.created_at - p.created_at < GROUP_WINDOW);
        list = list.push(message_row(chat, message, grouped));
        previous = Some(message);
    }
    for pending in &timeline.pending {
        list = list.push(pending_row(chat, pending));
    }

    scrollable(list).id(MESSAGES).anchor_bottom().on_scroll(Msg::Scrolled).height(Fill).width(Fill).into()
}

fn day_separator<'a>(message: &Message) -> Element<'a, Msg> {
    row![
        rule::horizontal(1),
        text(day_label(message.created_at)).size(12).font(theme::SEMIBOLD).color(theme::c().muted),
        rule::horizontal(1),
    ]
    .spacing(8)
    .align_y(Center)
    .padding([12, 16])
    .into()
}

fn message_row<'a>(chat: &'a Chat, message: &'a Message, grouped: bool) -> Element<'a, Msg> {
    let hovered = chat.hovered == Some(message.id);
    let mine = message.author.id == chat.me.id;
    let can_delete = mine || chat.is_owner(message.guild_id);
    let editing = chat.editing.as_ref().filter(|e| e.id == message.id);

    let body: Element<'a, Msg> = match editing {
        Some(edit) => column![
            container(
                text_input("", &edit.text)
                    .id(EDITOR)
                    .on_input(Msg::EditChanged)
                    .on_submit(Msg::SubmitEdit)
                    .padding(10)
                    .style(theme::bare_field),
            )
            .style(theme::rounded_fill(theme::c().input, 8.0)),
            text("Échap pour annuler • Entrée pour enregistrer").size(12).color(theme::c().muted),
        ]
        .spacing(4)
        .into(),
        None => match message.edited_at {
            // Inline marker right after the text, like Discord.
            Some(_) => rich_text![
                span::<(), _>(&message.content).color(theme::c().text),
                span("  (modifié)").size(11).color(theme::c().muted),
            ]
            .width(Fill)
            .into(),
            None => text(&message.content).color(theme::c().text).width(Fill).into(),
        },
    };

    let header = (!grouped).then(|| {
        row![
            text(&message.author.username).font(theme::SEMIBOLD).color(theme::user_color(message.author.id)),
            text(message_time(message.created_at)).size(12).color(theme::c().muted),
        ]
        .spacing(8)
        .align_y(Center)
    });

    let gutter: Element<'a, Msg> = if grouped {
        container(
            text(message.created_at.with_timezone(&chrono::Local).format("%H:%M").to_string())
                .size(10)
                .color(if hovered { theme::c().muted } else { iced::Color::TRANSPARENT }),
        )
        .width(40)
        .center_x(40)
        .into()
    } else {
        avatar(&message.author, 40.0)
    };

    let line = row![gutter, column![header, body].spacing(2).width(Fill)].spacing(16).padding(iced::Padding {
        top: if grouped { 2.0 } else { 14.0 },
        right: 16.0,
        bottom: 2.0,
        left: 16.0,
    });
    let base = container(line).width(Fill).style(theme::fill(if hovered {
        theme::c().message_hover
    } else {
        theme::c().chat
    }));

    // Floating toolbar drawn *over* the message, so hovering never reflows the text.
    let content: Element<'a, Msg> = if hovered && editing.is_none() {
        let action = |label: &'static str, danger: bool, msg: Msg| {
            button(text(label).size(12)).padding([3, 8]).style(theme::message_action(danger)).on_press(msg)
        };
        let toolbar = row![action("Copier", false, Msg::Copy(message.content.clone()))]
            .push(mine.then(|| action("Modifier", false, Msg::StartEdit(message.id))))
            .push(can_delete.then(|| action("Supprimer", true, Msg::Delete(message.id))))
            .spacing(2);
        let toolbar = container(toolbar).padding(2).style(theme::rounded_fill(theme::c().sidebar, 6.0));
        // Second stack layer: pinned top-right *inside* the row, so moving the pointer onto
        // it never leaves the message's hover area (a `float` lifted outside the row did).
        stack![base, container(toolbar).align_right(Fill).padding([2, 16])].into()
    } else {
        base.into()
    };

    mouse_area(content).on_enter(Msg::Hover(message.id)).on_exit(Msg::Unhover(message.id)).into()
}

/// Our own message while in flight (greyed) or after a failure (red, with actions).
fn pending_row<'a>(chat: &'a Chat, pending: &'a Pending) -> Element<'a, Msg> {
    let content = text(&pending.content).color(if pending.failed { theme::c().red } else { theme::c().muted });
    let status = pending.failed.then(|| {
        row![
            text("Échec de l'envoi.").size(12).color(theme::c().red),
            button(text("Réessayer").size(12))
                .padding(0)
                .style(theme::link)
                .on_press(Msg::Retry(pending.nonce.clone())),
            button(text("Supprimer").size(12))
                .padding(0)
                .style(theme::link)
                .on_press(Msg::Discard(pending.nonce.clone())),
        ]
        .spacing(8)
    });
    row![
        avatar(&chat.me, 40.0),
        column![text(&chat.me.username).font(theme::SEMIBOLD).color(theme::c().muted), content, status]
            .spacing(2)
            .width(Fill),
    ]
    .spacing(16)
    .padding([8, 16])
    .into()
}

fn composer<'a>(chat: &'a Chat, channel_name: &str) -> Element<'a, Msg> {
    let length = chat.composer.chars().count();
    let counter = (length > MESSAGE_MAX - 200).then(|| {
        text(format!("{}", MESSAGE_MAX as i64 - length as i64)).size(12).color(if length > MESSAGE_MAX {
            theme::c().red
        } else {
            theme::c().muted
        })
    });
    let input = text_input(&format!("Envoyer un message dans #{channel_name}"), &chat.composer)
        .id(COMPOSER)
        .on_input(Msg::ComposerChanged)
        .on_submit(Msg::Send)
        .padding(12)
        .size(15)
        .style(theme::bare_field);
    container(
        container(row![input, counter].align_y(Center).padding([0, 8]))
            .style(theme::rounded_fill(theme::c().input, 8.0)),
    )
    .padding(iced::Padding { top: 8.0, right: 16.0, bottom: 0.0, left: 16.0 })
    .into()
}

fn typing_line(chat: &Chat, channel_id: plume_lite_protocol::Snowflake) -> Element<'_, Msg> {
    let names = chat.store.typists(channel_id);
    let label = match names.as_slice() {
        [] => String::new(),
        [one] => format!("{one} est en train d'écrire…"),
        [a, b] => format!("{a} et {b} écrivent…"),
        _ => "Plusieurs personnes écrivent…".to_owned(),
    };
    container(text(label).size(12).font(theme::SEMIBOLD).color(theme::c().text)).padding([4, 20]).height(24).into()
}

fn empty_state(chat: &Chat) -> Element<'_, Msg> {
    let (title, subtitle) = if chat.guild.is_some() {
        ("Aucun salon", "Ce serveur n'a pas encore de salon textuel.")
    } else {
        ("Bienvenue sur Plume Lite 👋", "Créez votre premier serveur ou rejoignez celui d'un ami avec une invitation.")
    };
    let actions = chat.guild.is_none().then(|| {
        row![
            button(text("Créer un serveur").font(theme::SEMIBOLD))
                .padding([10, 16])
                .style(theme::primary)
                .on_press(Msg::OpenModal(ModalRequest::CreateGuild)),
            button(text("Rejoindre un serveur").font(theme::SEMIBOLD))
                .padding([10, 16])
                .style(theme::ghost)
                .on_press(Msg::OpenModal(ModalRequest::JoinGuild)),
        ]
        .spacing(12)
    });
    center(
        column![
            text(title).size(24).font(theme::BOLD).color(theme::c().bright),
            text(subtitle).color(theme::c().muted),
            space().height(8),
            actions
        ]
        .spacing(8)
        .align_x(Center)
        .max_width(420),
    )
    .into()
}
