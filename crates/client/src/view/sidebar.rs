use iced::widget::{Column, button, column, container, row, rule, scrollable, space, text, tooltip};
use iced::{Center, Element, Fill};

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
pub fn channels(chat: &Chat) -> Element<'_, Msg> {
    let body: Element<'_, Msg> = match chat.guild.and_then(|id| chat.store.guild(id)) {
        Some(guild) => {
            let is_owner = chat.is_owner(guild.id);
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
                let mut items =
                    column![item("Inviter des gens", false, Msg::OpenModal(ModalRequest::Invite(guild.id)))];
                if is_owner {
                    items = items
                        .push(item("Créer un salon", false, Msg::OpenModal(ModalRequest::CreateChannel(guild.id))))
                        .push(item("Supprimer le serveur", true, Msg::OpenModal(ModalRequest::DeleteGuild(guild.id))));
                } else {
                    items = items.push(item(
                        "Quitter le serveur",
                        true,
                        Msg::OpenModal(ModalRequest::LeaveGuild(guild.id)),
                    ));
                }
                container(items.spacing(2).padding(6)).style(theme::rounded_fill(theme::c().rail, 6.0)).padding([0, 8])
            });

            let mut list = Column::new().spacing(2).padding([8, 8]);
            list = list.push(
                row![text("SALONS TEXTUELS").size(12).font(theme::BOLD).color(theme::c().muted).width(Fill)]
                    .push(is_owner.then(|| {
                        button(text("+").size(16).color(theme::c().muted))
                            .padding([0, 6])
                            .style(theme::ghost)
                            .on_press(Msg::OpenModal(ModalRequest::CreateChannel(guild.id)))
                    }))
                    .align_y(Center)
                    .padding([8, 8]),
            );
            for channel in chat.store.channels_of(guild.id) {
                let selected = chat.channel == Some(channel.id);
                let unread = chat.store.is_unread(channel.id);
                let label =
                    row![
                        text("#").size(18).color(theme::c().muted),
                        text(&channel.name).font(if unread { theme::BOLD } else { iced::Font::DEFAULT }).width(Fill),
                    ]
                    .push((unread && !selected).then(|| {
                        container(space()).width(8).height(8).style(theme::rounded_fill(theme::c().pink, 4.0))
                    }))
                    .spacing(6)
                    .align_y(Center);
                list = list.push(
                    button(label)
                        .width(Fill)
                        .padding([6, 8])
                        .style(theme::list_item(selected, unread))
                        .on_press(Msg::SelectChannel(channel.id)),
                );
            }

            column![header, container(rule::horizontal(1)).width(Fill), menu, scrollable(list).height(Fill),].into()
        }
        None => {
            container(text("Aucun serveur pour l'instant.").color(theme::c().muted)).center(Fill).padding(16).into()
        }
    };

    container(column![body, user_panel(chat)])
        .width(SIDEBAR_WIDTH)
        .height(Fill)
        .style(theme::fill(theme::c().sidebar))
        .into()
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
