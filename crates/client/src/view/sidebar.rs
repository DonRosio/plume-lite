use iced::widget::{Column, button, column, container, row, rule, scrollable, space, stack, text, tooltip};
use iced::{Border, Center, Color, Element, Fill};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::model::{
    Category, Channel, ChannelKind, MENTIONS_COUNTED_MAX, NotificationLevel, User, VoiceState,
};
use plume_lite_protocol::permissions::{Permissions, is_private};

use super::{avatar, initials};
use crate::app::{Chat, Connection, ModalRequest, Msg};
use crate::theme;

const RAIL_WIDTH: f32 = 72.0;
const SIDEBAR_WIDTH: f32 = 240.0;

/// Left-most column: one icon per guild, then "create" and "join".
pub fn rail(chat: &Chat) -> Element<'_, Msg> {
    let mut icons = Column::new().spacing(8).align_x(Center).width(Fill);
    for guild in chat.store.guilds() {
        let selected = chat.guild == Some(guild.id);
        let unread = !selected && chat.store.guild_has_unread(guild.id);
        // Discord's "pill": tall when selected, a dot when there is something unread.
        let pill_height = if selected {
            40.0
        } else if unread {
            8.0
        } else {
            0.0
        };
        let pill_color = if selected { theme::c().bright } else { theme::c().pink };
        let pill = container(space()).width(4).height(pill_height).style(theme::rounded_fill(pill_color, 2.0));
        let icon = button(container(text(initials(&guild.name)).size(16).font(theme::SEMIBOLD)).center(48))
            .padding(0)
            .style(theme::guild_icon(selected))
            .on_press(Msg::SelectGuild(guild.id));
        // Unread mentions: a counter in the corner of the icon.
        let icon: Element<'_, Msg> = match chat.store.guild_mentions(guild.id) {
            0 => icon.into(),
            mentions => {
                stack![icon, container(mention_badge(mentions)).width(48).height(48).align_right(48).align_bottom(48),]
                    .into()
            }
        };
        let entry = row![pill, space().width(8), tooltip(icon, hint(&guild.name), tooltip::Position::Right)]
            .align_y(Center)
            .width(Fill);
        icons = icons.push(entry);
    }

    let action = |label: &'static str, hint_text: &'static str, msg: Msg| {
        tooltip(
            button(container(text(label).size(22)).center(48)).padding(0).style(theme::rail_action).on_press(msg),
            hint(hint_text),
            tooltip::Position::Right,
        )
    };

    let content = column![
        icons,
        container(rule::horizontal(2)).width(32),
        action("+", "Créer un serveur", Msg::OpenModal(ModalRequest::CreateGuild)),
        action("→", "Rejoindre un serveur", Msg::OpenModal(ModalRequest::JoinGuild)),
    ]
    .spacing(8)
    .padding([12, 0])
    .align_x(Center)
    .width(Fill);

    container(
        scrollable(content).height(Fill).direction(scrollable::Direction::Vertical(scrollable::Scrollbar::hidden())),
    )
    .width(RAIL_WIDTH)
    .height(Fill)
    .style(theme::fill(theme::c().rail))
    .into()
}

/// Guild header (with its menu), channel list and the user panel.
pub fn channels<'a>(chat: &'a Chat) -> Element<'a, Msg> {
    let body: Element<'_, Msg> = match chat.guild.and_then(|id| chat.store.guild(id)) {
        Some(guild) => {
            let me = chat.standing(guild.id);
            let manages_channels = me.can(Permissions::MANAGE_CHANNELS);
            let header = button(
                row![
                    column![
                        text(&guild.name).font(theme::BOLD).color(theme::c().bright),
                        text(format!("{} membre(s)", chat.store.member_count(guild.id)))
                            .size(11)
                            .color(theme::c().muted),
                    ]
                    .width(Fill),
                    text(if chat.guild_menu { "×" } else { "⌄" }).color(theme::c().bright)
                ]
                .align_y(Center),
            )
            .padding([8, 16])
            .width(Fill)
            .style(theme::list_item(chat.guild_menu, true))
            .on_press(Msg::ToggleGuildMenu);

            let menu = chat.guild_menu.then(|| {
                let item = |label: &'static str, danger: bool, msg: Msg| {
                    button(text(label).size(14).color(if danger { theme::c().red } else { theme::c().text }))
                        .width(Fill)
                        .padding([8, 10])
                        .style(theme::ghost)
                        .on_press(msg)
                };
                let mut items = Column::new().push(
                    me.can(Permissions::CREATE_INVITE)
                        .then(|| item("Inviter des gens", false, Msg::OpenModal(ModalRequest::Invite(guild.id)))),
                );
                // Which messages of this guild notify us (each channel can override it).
                let current = chat.store.notification_setting(guild.id).unwrap_or_default();
                items = items.push(text("NOTIFICATIONS").size(11).font(theme::BOLD).color(theme::c().muted));
                for (level, label) in [
                    (NotificationLevel::All, "Tous les messages"),
                    (NotificationLevel::Mentions, "Mentions seulement"),
                    (NotificationLevel::Nothing, "Rien"),
                ] {
                    let selected = current == level;
                    let line = row![text(label).size(14).width(Fill)].push(selected.then(|| text("✓").size(14)));
                    let level = (level != NotificationLevel::All).then_some(level);
                    items = items.push(
                        button(line)
                            .width(Fill)
                            .padding([6, 10])
                            .style(theme::list_item(selected, false))
                            .on_press(Msg::SetNotifications { target_id: guild.id, level }),
                    );
                }
                let create =
                    |kind| Msg::OpenModal(ModalRequest::CreateChannel { guild_id: guild.id, kind, category: None });
                let arrange = if chat.arranging { "Terminer l'organisation" } else { "Organiser les salons" };
                let mut manage: Vec<Element<'a, Msg>> = Vec::new();
                if me.can(Permissions::MANAGE_GUILD) {
                    manage.push(
                        item("Paramètres du serveur", false, Msg::OpenModal(ModalRequest::GuildSettings(guild.id)))
                            .into(),
                    );
                }
                if me.can(Permissions::MANAGE_CHANNELS) {
                    manage.extend([
                        item("Créer un salon textuel", false, create(ChannelKind::Text)).into(),
                        item("Créer un salon vocal", false, create(ChannelKind::Voice)).into(),
                        item("Créer une catégorie", false, Msg::OpenModal(ModalRequest::CreateCategory(guild.id)))
                            .into(),
                        item(arrange, false, Msg::ToggleArrange).into(),
                    ]);
                }
                if me.can(Permissions::MANAGE_ROLES) {
                    manage.push(item("Rôles", false, Msg::OpenModal(ModalRequest::Roles(guild.id))).into());
                }
                if me.can(Permissions::VIEW_AUDIT_LOG) {
                    manage.push(item("Journal", false, Msg::OpenModal(ModalRequest::AuditLog(guild.id))).into());
                }
                if me.can(Permissions::BAN_MEMBERS) {
                    manage.push(item("Membres bannis", false, Msg::OpenModal(ModalRequest::Bans(guild.id))).into());
                }
                if me.owner {
                    manage.push(
                        item("Supprimer le serveur", true, Msg::OpenModal(ModalRequest::DeleteGuild(guild.id))).into(),
                    );
                }
                if !manage.is_empty() {
                    items = items
                        .push(text("GESTION").size(11).font(theme::BOLD).color(theme::c().muted))
                        .push(Column::with_children(manage).spacing(2));
                }
                if !me.owner {
                    items = items.push(item(
                        "Quitter le serveur",
                        true,
                        Msg::OpenModal(ModalRequest::LeaveGuild(guild.id)),
                    ));
                }
                container(items.spacing(2).padding(6)).style(theme::rounded_fill(theme::c().rail, 6.0)).padding([0, 8])
            });

            let arranging = manages_channels && chat.arranging;
            let sections = chat.store.sections(guild.id);
            let has_categories = sections.len() > 1;
            let mut list = Column::new().spacing(2).padding([8, 8]);
            if arranging {
                list = list.push(
                    container(
                        row![
                            text("Flèches : changer l'ordre ; ✏ : renommer ou changer de catégorie.")
                                .size(12)
                                .color(theme::c().muted)
                                .width(Fill),
                            button(text("Terminé").size(13))
                                .padding([4, 8])
                                .style(theme::primary)
                                .on_press(Msg::ToggleArrange),
                        ]
                        .spacing(6)
                        .align_y(Center),
                    )
                    .padding([6, 8]),
                );
            }
            for section in &sections {
                let (text_channels, voice_channels): (Vec<&Channel>, Vec<&Channel>) =
                    section.channels.iter().partition(|c| c.kind != ChannelKind::Voice);
                match section.category {
                    // Without a category: as before categories existed, a heading per kind.
                    None => {
                        if !text_channels.is_empty() || (manages_channels && !has_categories) {
                            list = list.push(kind_heading(
                                guild.id,
                                manages_channels,
                                "SALONS TEXTUELS",
                                ChannelKind::Text,
                            ));
                        }
                        list = push_channels(list, chat, guild.id, arranging, &text_channels, true);
                        if !voice_channels.is_empty() || (manages_channels && !has_categories) {
                            list = list.push(kind_heading(
                                guild.id,
                                manages_channels,
                                "SALONS VOCAUX",
                                ChannelKind::Voice,
                            ));
                        }
                        list = push_channels(list, chat, guild.id, arranging, &voice_channels, true);
                    }
                    Some(category) => {
                        let collapsed = chat.collapsed.contains(&category.id) && !arranging;
                        list = list.push(category_heading(
                            chat,
                            guild.id,
                            category,
                            manages_channels,
                            arranging,
                            collapsed,
                        ));
                        // Folded: only what is in use stays visible.
                        let visible = |channels: Vec<&'a Channel>| -> Vec<&'a Channel> {
                            if !collapsed {
                                return channels;
                            }
                            channels
                                .into_iter()
                                .filter(|c| {
                                    chat.channel == Some(c.id)
                                        || chat.voice.as_ref().is_some_and(|v| v.channel_id == c.id)
                                })
                                .collect()
                        };
                        list = push_channels(list, chat, guild.id, arranging, &visible(text_channels), !collapsed);
                        list = push_channels(list, chat, guild.id, arranging, &visible(voice_channels), !collapsed);
                    }
                }
            }

            column![header, container(rule::horizontal(1)).width(Fill), menu, scrollable(list).height(Fill),].into()
        }
        None => {
            container(text("Aucun serveur pour l'instant.").color(theme::c().muted)).center(Fill).padding(16).into()
        }
    };

    container(column![body].push(voice_panel(chat)).push(user_panel(chat)))
        .width(SIDEBAR_WIDTH)
        .height(Fill)
        .style(theme::fill(theme::c().sidebar))
        .into()
}

/// "SALONS TEXTUELS" or "SALONS VOCAUX", with "+" for those who manage channels.
fn kind_heading<'a>(
    guild_id: Snowflake,
    manages_channels: bool,
    title: &'a str,
    kind: ChannelKind,
) -> Element<'a, Msg> {
    row![text(title).size(12).font(theme::BOLD).color(theme::c().muted).width(Fill)]
        .push(manages_channels.then(|| {
            button(text("+").size(16).color(theme::c().muted))
                .padding([0, 6])
                .style(theme::ghost)
                .on_press(Msg::OpenModal(ModalRequest::CreateChannel { guild_id, kind, category: None }))
        }))
        .align_y(Center)
        .padding([8, 8])
        .into()
}

/// A category's name (click: fold or unfold), with "+" for those who manage channels, or the arrows, rename
/// and delete buttons while arranging.
fn category_heading<'a>(
    chat: &'a Chat,
    guild_id: Snowflake,
    category: &'a Category,
    manages_channels: bool,
    arranging: bool,
    collapsed: bool,
) -> Element<'a, Msg> {
    let c = theme::c();
    let title = button(
        row![
            text(if collapsed { "›" } else { "⌄" }).size(12).color(c.muted),
            text(category.name.to_uppercase()).size(12).font(theme::BOLD).color(c.muted),
        ]
        .spacing(6)
        .align_y(Center),
    )
    .width(Fill)
    .padding([2, 0])
    .style(theme::ghost)
    .on_press_maybe((!arranging).then_some(Msg::ToggleCategory(category.id)));
    let mut heading =
        row![title].spacing(2).align_y(Center).padding(iced::Padding { top: 10.0, ..iced::Padding::from([2, 4]) });
    if arranging {
        let order = chat.store.categories_of(guild_id);
        let index = order.iter().position(|k| k.id == category.id).unwrap_or(0);
        heading = heading
            .push(small_button("▲", (index > 0).then_some(Msg::MoveCategory { category_id: category.id, up: true })))
            .push(small_button(
                "▼",
                (index + 1 < order.len()).then_some(Msg::MoveCategory { category_id: category.id, up: false }),
            ))
            .push(small_button("✏", Some(Msg::OpenModal(ModalRequest::RenameCategory(category.id)))))
            .push(small_button("🗑", Some(Msg::OpenModal(ModalRequest::DeleteCategory(category.id)))));
    } else if manages_channels {
        heading = heading.push(small_button(
            "+",
            Some(Msg::OpenModal(ModalRequest::CreateChannel {
                guild_id,
                kind: ChannelKind::Text,
                category: Some(category.id),
            })),
        ));
    }
    heading.into()
}

/// An arrow or icon of the channel list; faded when it does nothing (first or last).
fn small_button(label: &str, msg: Option<Msg>) -> Element<'_, Msg> {
    let muted = theme::c().muted;
    let color = if msg.is_some() { muted } else { Color { a: 0.3, ..muted } };
    button(text(label).size(12).color(color)).padding([2, 5]).style(theme::ghost).on_press_maybe(msg).into()
}

/// Channels of one kind under one heading: text channels to open, voice channels to join
/// (with who is in them). While arranging, each gets its arrows and an edit button.
fn push_channels<'a>(
    mut list: Column<'a, Msg>,
    chat: &'a Chat,
    guild_id: Snowflake,
    arranging: bool,
    channels: &[&'a Channel],
    complete: bool,
) -> Column<'a, Msg> {
    for (index, channel) in channels.iter().enumerate() {
        let entry: Element<'a, Msg> = if channel.kind == ChannelKind::Voice {
            let joined = chat.voice.as_ref().is_some_and(|v| v.channel_id == channel.id);
            let label = row![text("🔊").size(14), text(&channel.name)]
                .push(private_mark(channel))
                .push(space::horizontal())
                .spacing(6)
                .align_y(Center);
            button(label)
                .width(Fill)
                .padding([6, 8])
                .style(theme::list_item(joined, false))
                .on_press(Msg::JoinVoice(channel.id))
                .into()
        } else {
            text_channel(chat, channel)
        };
        // Arrows only when the whole list of this kind is shown (not folded).
        let entry: Element<'a, Msg> = if arranging && complete {
            row![
                entry,
                small_button("▲", (index > 0).then_some(Msg::MoveChannel { channel_id: channel.id, up: true })),
                small_button(
                    "▼",
                    (index + 1 < channels.len()).then_some(Msg::MoveChannel { channel_id: channel.id, up: false }),
                ),
                small_button("✏", Some(Msg::OpenModal(ModalRequest::EditChannel(channel.id)))),
            ]
            .spacing(2)
            .align_y(Center)
            .into()
        } else {
            entry
        };
        list = list.push(entry);
        if channel.kind == ChannelKind::Voice {
            for state in chat.store.voice_members(channel.id) {
                list = list.push(voice_member(chat, guild_id, state));
            }
        }
    }
    list
}

/// A small lock on private channels (hidden from @everyone).
fn private_mark<'a>(channel: &Channel) -> Option<Element<'a, Msg>> {
    is_private(channel.guild_id, &channel.overwrites).then(|| text("🔒").size(14).color(theme::c().muted).into())
}

/// A text channel: bold when unread, with a dot or the number of mentions.
fn text_channel<'a>(chat: &'a Chat, channel: &'a Channel) -> Element<'a, Msg> {
    let selected = chat.channel == Some(channel.id);
    let unread = chat.store.is_unread(channel.id);
    let label = row![
        text("#").size(18).color(theme::c().muted),
        text(&channel.name).font(if unread { theme::BOLD } else { theme::REGULAR }),
    ]
    .push(private_mark(channel))
    .push(space::horizontal())
    .push((!selected).then(|| match chat.store.mentions(channel.id) {
        0 if unread => Some(container(space()).width(8).height(8).style(theme::rounded_fill(theme::c().pink, 4.0))),
        0 => None,
        mentions => Some(mention_badge(mentions)),
    }))
    .spacing(6)
    .align_y(Center);
    button(label)
        .width(Fill)
        .padding([6, 8])
        .style(theme::list_item(selected, unread))
        .on_press(Msg::SelectChannel(channel.id))
        .into()
}

/// Someone in a voice channel: who, whether they are speaking, muted or deafened.
fn voice_member<'a>(chat: &'a Chat, guild_id: Snowflake, state: &VoiceState) -> Element<'a, Msg> {
    let c = theme::c();
    let name = chat.store.user_name(guild_id, state.user_id).unwrap_or_else(|| "…".to_owned());
    let speaking = chat.voice.as_ref().is_some_and(|v| v.speaking.contains(&state.user_id));
    let user = User { id: state.user_id, username: name.clone() };
    // A green ring around the avatar while speaking.
    let ring = if speaking { c.green } else { Color::TRANSPARENT };
    let picture = container(avatar(&user, 20.0)).padding(2).style(move |_| container::Style {
        border: Border { color: ring, width: 2.0, radius: 12.0.into() },
        ..container::Style::default()
    });
    let badge = if state.server_mute {
        Some(text("rendu muet").size(11).color(c.red))
    } else if state.self_deaf {
        Some(text("son coupé").size(11).color(c.muted))
    } else if state.self_mute {
        Some(text("muet").size(11).color(c.muted))
    } else {
        None
    };
    let line = row![picture, text(name).size(13).color(if speaking { c.bright } else { c.text }).width(Fill)]
        .push(badge)
        .spacing(6)
        .align_y(Center);
    let moderate = (chat.can(guild_id, Permissions::MUTE_MEMBERS) || chat.can(guild_id, Permissions::MOVE_MEMBERS))
        && chat.outranks(guild_id, state.user_id);
    button(line)
        .width(Fill)
        .padding(iced::Padding { left: 28.0, ..iced::Padding::from([2, 8]) })
        .style(theme::list_item(false, speaking))
        .on_press_maybe(
            moderate.then_some(Msg::OpenModal(ModalRequest::VoiceMember { guild_id, user_id: state.user_id })),
        )
        .into()
}

/// While in voice: where, and the microphone, sound and leave buttons.
fn voice_panel(chat: &Chat) -> Option<Element<'_, Msg>> {
    let session = chat.voice.as_ref()?;
    let c = theme::c();
    let channel = chat.store.channel(session.channel_id).map_or("…", |ch| ch.name.as_str());
    let (status, color) =
        if session.commands.is_some() { ("Vocal connecté", c.green) } else { ("Connexion au vocal…", c.yellow) };
    let toggle = |label: &'static str, off: bool, msg: Msg| {
        button(text(label).size(13).width(Fill).center())
            .width(Fill)
            .padding([6, 4])
            .style(if off { theme::danger } else { theme::ghost })
            .on_press(msg)
    };
    let mic_off = chat.voice_mute || chat.voice_deaf;
    Some(
        container(
            column![
                column![
                    text(status).size(13).font(theme::SEMIBOLD).color(color),
                    text(format!("🔊 {channel}")).size(12).color(c.muted),
                ]
                .spacing(2),
                row![
                    toggle(if mic_off { "Micro coupé" } else { "Micro" }, mic_off, Msg::ToggleMute),
                    toggle(if chat.voice_deaf { "Son coupé" } else { "Son" }, chat.voice_deaf, Msg::ToggleDeaf),
                    button(text("Quitter").size(13).width(Fill).center())
                        .width(Fill)
                        .padding([6, 4])
                        .style(theme::danger)
                        .on_press(Msg::LeaveVoice),
                ]
                .spacing(6),
            ]
            .spacing(8),
        )
        .padding([10, 10])
        .width(Fill)
        .style(theme::fill(c.panel))
        .into(),
    )
}

fn user_panel(chat: &Chat) -> Element<'_, Msg> {
    let (status, color) = match &chat.connection {
        Connection::Online => ("En ligne".to_owned(), theme::c().green),
        Connection::Connecting => ("Connexion…".to_owned(), theme::c().yellow),
        Connection::Reconnecting { retry_in, .. } => {
            (format!("Reconnexion ({} s)…", retry_in.as_secs().max(1)), theme::c().red)
        }
    };
    let dot = container(space()).width(10).height(10).style(theme::rounded_fill(color, 5.0));
    let identity = column![
        text(&chat.me.username).size(14).font(theme::SEMIBOLD).color(theme::c().bright),
        row![dot, text(status).size(12).color(theme::c().muted)].spacing(6).align_y(Center),
    ]
    .spacing(2)
    .width(Fill);

    container(
        row![
            avatar(&chat.me, 32.0),
            identity,
            tooltip(
                button(text(if chat.do_not_disturb { "🔕" } else { "🔔" }).size(16))
                    .padding([4, 8])
                    .style(theme::ghost)
                    .on_press(Msg::ToggleDoNotDisturb),
                hint(if chat.do_not_disturb {
                    "Ne pas déranger : aucune notification (cliquer pour les rétablir)"
                } else {
                    "Ne pas déranger"
                }),
                tooltip::Position::Top,
            ),
            tooltip(
                button(text("⚙").size(16)).padding([4, 8]).style(theme::ghost).on_press(Msg::OpenSettings),
                hint("Paramètres"),
                tooltip::Position::Top,
            ),
            tooltip(
                button(text(if theme::is_dark() { "☀" } else { "☾" }).size(16))
                    .padding([4, 8])
                    .style(theme::ghost)
                    .on_press(Msg::ToggleTheme),
                hint(if theme::is_dark() { "Thème clair" } else { "Thème sombre" }),
                tooltip::Position::Top,
            ),
            tooltip(
                button(text("⏻").size(16)).padding([4, 8]).style(theme::ghost).on_press(Msg::Logout),
                hint("Se déconnecter"),
                tooltip::Position::Top,
            )
        ]
        .spacing(8)
        .align_y(Center),
    )
    .padding([10, 8])
    .width(Fill)
    .style(theme::fill(theme::c().panel))
    .into()
}

fn hint(label: &str) -> Element<'_, Msg> {
    container(text(label).size(13).color(theme::c().bright))
        .padding([6, 10])
        .style(theme::rounded_fill(theme::c().rail, 4.0))
        .into()
}

/// "3" (or "99+") in a pink pill: unread messages that mention us.
fn mention_badge<'a>(mentions: u32) -> iced::widget::Container<'a, Msg> {
    let label = if mentions >= MENTIONS_COUNTED_MAX { "99+".to_owned() } else { mentions.to_string() };
    container(text(label).size(11).font(theme::BOLD).color(theme::c().on_fill))
        .padding([1, 6])
        .style(theme::rounded_fill(theme::c().pink, 8.0))
}
