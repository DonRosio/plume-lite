use chrono::TimeDelta;
use iced::keyboard::{Key, key::Named};
use iced::widget::text_editor::{Binding, KeyPress};
use iced::widget::{
    Column, button, center, column, container, mouse_area, row, rule, scrollable, space, span, stack, text, text_editor,
};
use iced::{Center, Element, Fill};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::markup::{self as protocol_markup, Block};
use plume_lite_protocol::model::{Message, Replied};
use plume_lite_protocol::validate::MESSAGE_MAX;

use super::markup::{self, Context};
use super::{avatar, day_label, message_time};
use crate::app::{COMPOSER, Chat, Connection, EDITOR, MESSAGES, ModalRequest, Msg, Suggestions};
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

    // Optional parts sit in containers that are always there: a column skips empty children,
    // and a widget that moves in the tree loses its state (focus, scroll position).
    column![
        header,
        container(rule::horizontal(1)).width(Fill),
        container(banner),
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
        // A reply always shows who wrote it, under what it answers.
        let grouped = !new_day
            && message.reply_to.is_none()
            && previous
                .is_some_and(|p| p.author.id == message.author.id && message.created_at - p.created_at < GROUP_WINDOW);
        list = list.push(message_row(chat, message, timeline.blocks(message.id), grouped));
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

fn message_row<'a>(chat: &'a Chat, message: &'a Message, blocks: &'a [Block], grouped: bool) -> Element<'a, Msg> {
    let c = theme::c();
    let hovered = chat.hovered == Some(message.id);
    let mine = message.author.id == chat.me.id;
    let can_delete = mine || chat.is_owner(message.guild_id);
    let editing = chat.editing.as_ref().filter(|e| e.id == message.id);
    let ctx = Context {
        chat,
        guild_id: message.guild_id,
        message_id: message.id,
        everyone: message.mention_everyone,
        color: None,
    };

    let body: Element<'a, Msg> = match editing {
        Some(edit) => column![
            container(
                text_editor(&edit.text)
                    .id(EDITOR)
                    .on_action(Msg::EditAction)
                    .key_binding(edit_binding)
                    .padding(10)
                    .max_height(320)
                    .style(theme::bare_editor),
            )
            .style(theme::rounded_fill(c.input, 8.0)),
            text("Échap pour annuler • Entrée pour enregistrer • Maj+Entrée pour aller à la ligne")
                .size(12)
                .color(c.muted),
        ]
        .spacing(4)
        .into(),
        // " (modifié)" right after the text, like Discord.
        None => markup::view(ctx, blocks, message.edited_at.map(|_| span("  (modifié)").size(11).color(c.muted))),
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

    let reply = message.reply_to.as_ref().map(|replied| reply_line(ctx, replied));
    let line = row![gutter, column![header, body].spacing(2).width(Fill)].spacing(16).padding(iced::Padding {
        top: if grouped || reply.is_some() { 2.0 } else { 14.0 },
        right: 16.0,
        bottom: 2.0,
        left: 16.0,
    });
    let mentioned = mentions_me(chat, message, blocks);
    let background = if mentioned {
        c.mention
    } else if hovered {
        c.message_hover
    } else {
        c.chat
    };
    let base = container(column![reply, line]).width(Fill).style(theme::fill(background));
    // A bar on the left of the messages that mention us, drawn over the margin.
    let base: Element<'a, Msg> = if mentioned {
        stack![base, container(space()).width(3).height(Fill).style(theme::fill(c.pink))].into()
    } else {
        base.into()
    };

    // Floating toolbar drawn *over* the message, so hovering never reflows the text.
    let content: Element<'a, Msg> = if hovered && editing.is_none() {
        let action = |label: &'static str, danger: bool, msg: Msg| {
            button(text(label).size(12)).padding([3, 8]).style(theme::message_action(danger)).on_press(msg)
        };
        let toolbar = row![
            action("Répondre", false, Msg::Reply(message.id)),
            action("Copier", false, Msg::Copy(message.content.clone()))
        ]
        .push(mine.then(|| action("Modifier", false, Msg::StartEdit(message.id))))
        .push(can_delete.then(|| action("Supprimer", true, Msg::Delete(message.id))))
        .spacing(2);
        let toolbar = container(toolbar).padding(2).style(theme::rounded_fill(theme::c().sidebar, 6.0));
        // Second stack layer: pinned top-right *inside* the row, so moving the pointer onto
        // it never leaves the message's hover area (a `float` lifted outside the row did).
        stack![base, container(toolbar).align_right(Fill).padding([2, 16])].into()
    } else {
        base
    };

    mouse_area(content).on_enter(Msg::Hover(message.id)).on_exit(Msg::Unhover(message.id)).into()
}

/// Whether a message is for us: it names us, the guild owner wrote `@everyone`, or it answers
/// one of our messages.
fn mentions_me(chat: &Chat, message: &Message, blocks: &[Block]) -> bool {
    if message.author.id == chat.me.id {
        return false;
    }
    let (users, everyone) = protocol_markup::mentions(blocks);
    let answers_me = message.reply_to.as_ref().and_then(|r| r.author.as_ref()).is_some_and(|a| a.id == chat.me.id);
    users.contains(&chat.me.id) || (everyone && message.mention_everyone) || answers_me
}

/// "↪ bob: the beginning of the answered message", above a reply.
fn reply_line<'a>(ctx: Context<'a>, replied: &'a Replied) -> Element<'a, Msg> {
    let c = theme::c();
    let line = match &replied.author {
        Some(author) => row![
            text("↪").size(13).color(c.muted),
            text(&author.username).size(13).font(theme::SEMIBOLD).color(theme::user_color(author.id)),
            markup::excerpt(ctx, &replied.excerpt),
        ],
        None => row![
            text("↪").size(13).color(c.muted),
            text("Message original supprimé").size(13).font(theme::ITALIC).color(c.muted)
        ],
    };
    // Lined up with the text of the message, past the avatar.
    container(line.spacing(6).align_y(Center))
        .padding(iced::Padding { top: 10.0, right: 16.0, bottom: 0.0, left: 72.0 })
        .into()
}

/// Our own message while in flight (greyed) or after a failure (red, with actions).
fn pending_row<'a>(chat: &'a Chat, pending: &'a Pending) -> Element<'a, Msg> {
    let c = theme::c();
    let ctx = Context {
        chat,
        guild_id: chat.guild.unwrap_or(Snowflake(0)),
        message_id: Snowflake(0),
        everyone: false,
        color: Some(if pending.failed { c.red } else { c.muted }),
    };
    let answered = pending.reply_to.and_then(|id| chat.channel.and_then(|channel| chat.store.message(channel, id)));
    let reply = answered.map(|m| text(format!("↪ en réponse à {}", m.author.username)).size(12).color(c.muted));
    let content = markup::view(ctx, &pending.blocks, None);
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
        column![text(&chat.me.username).font(theme::SEMIBOLD).color(theme::c().muted), reply, content, status]
            .spacing(2)
            .width(Fill),
    ]
    .spacing(16)
    .padding([8, 16])
    .into()
}

fn composer<'a>(chat: &'a Chat, channel_name: &str) -> Element<'a, Msg> {
    let c = theme::c();
    let length = chat.composer.text().chars().count();
    let counter = (length > MESSAGE_MAX - 200).then(|| {
        text(format!("{}", MESSAGE_MAX as i64 - length as i64)).size(12).color(if length > MESSAGE_MAX {
            c.red
        } else {
            c.muted
        })
    });
    let suggesting = chat.suggestions.is_some();
    let input = text_editor(&chat.composer)
        .id(COMPOSER)
        .placeholder(format!("Envoyer un message dans #{channel_name}"))
        .on_action(Msg::ComposerAction)
        .key_binding(move |press| composer_binding(press, suggesting))
        .padding(12)
        .size(15)
        .max_height(240)
        .style(theme::bare_editor);
    let field =
        container(row![input, counter].align_y(Center).padding([0, 8])).style(theme::rounded_fill(c.input, 8.0));
    let above = column![chat.suggestions.as_ref().map(suggestions), reply_bar(chat)].spacing(6);
    let gap = if chat.suggestions.is_some() || chat.replying.is_some() { 6 } else { 0 };
    // `above` stays in the tree even when empty, so the editor keeps its focus when it changes.
    container(column![container(above), field].spacing(gap))
        .padding(iced::Padding { top: 8.0, right: 16.0, bottom: 0.0, left: 16.0 })
        .into()
}

/// Enter sends, Shift+Enter breaks the line; while names are offered, Enter or Tab picks one
/// and the arrows move between them. Escape keeps the focus here: the app closes the
/// suggestions, then the reply (`Msg::Escape`).
fn composer_binding(press: KeyPress, suggesting: bool) -> Option<Binding<Msg>> {
    let named = match &press.key {
        Key::Named(named) => Some(*named),
        _ => None,
    };
    match named {
        Some(Named::Enter) if press.modifiers.shift() => Some(Binding::Enter),
        Some(Named::Enter | Named::Tab) if suggesting => Some(Binding::Custom(Msg::PickSuggestion)),
        Some(Named::Enter) => Some(Binding::Custom(Msg::Send)),
        Some(Named::ArrowUp) if suggesting => Some(Binding::Custom(Msg::MoveSuggestion(-1))),
        Some(Named::ArrowDown) if suggesting => Some(Binding::Custom(Msg::MoveSuggestion(1))),
        Some(Named::Escape) => Some(Binding::Custom(Msg::Ignore)),
        _ => Binding::from_key_press(press),
    }
}

/// Enter saves, Shift+Enter breaks the line.
fn edit_binding(press: KeyPress) -> Option<Binding<Msg>> {
    match &press.key {
        Key::Named(Named::Enter) if !press.modifiers.shift() => Some(Binding::Custom(Msg::SubmitEdit)),
        _ => Binding::from_key_press(press),
    }
}

/// The names matching the `@…` being typed.
fn suggestions(suggestions: &Suggestions) -> Element<'_, Msg> {
    let c = theme::c();
    let mut list = Column::new().spacing(2);
    for (i, name) in suggestions.names.iter().enumerate() {
        let note = (name == "everyone").then(|| text("prévient tout le serveur").size(12).color(c.muted));
        let label = row![text(format!("@{name}")).size(14).font(theme::SEMIBOLD).color(c.text)].push(note).spacing(8);
        list = list.push(
            button(label)
                .width(Fill)
                .padding([4, 10])
                .style(theme::list_item(i == suggestions.selected, false))
                .on_press(Msg::PickMention(name.clone())),
        );
    }
    container(column![text("MEMBRES").size(11).font(theme::SEMIBOLD).color(c.muted), list].spacing(6))
        .padding(8)
        .width(Fill)
        .style(theme::rounded_fill(c.sidebar, 8.0))
        .into()
}

/// "En réponse à bob ✕" above the composer.
fn reply_bar(chat: &Chat) -> Option<Element<'_, Msg>> {
    let c = theme::c();
    let message = chat.store.message(chat.channel?, chat.replying?)?;
    Some(
        container(
            row![
                text("En réponse à").size(13).color(c.muted),
                text(&message.author.username)
                    .size(13)
                    .font(theme::SEMIBOLD)
                    .color(theme::user_color(message.author.id))
                    .width(Fill),
                button(text("✕").size(13)).padding([0, 6]).style(theme::ghost).on_press(Msg::CancelReply),
            ]
            .spacing(6)
            .align_y(Center),
        )
        .padding([4, 12])
        .style(theme::rounded_fill(c.sidebar, 6.0))
        .into(),
    )
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
