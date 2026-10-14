use iced::widget::{Column, button, container, row, scrollable, space, stack, text};
use iced::{Border, Center, Color, Element, Fill};
use plume_lite_protocol::model::{Member, Status};

use super::avatar;
use crate::app::{Chat, ModalRequest, Msg};
use crate::theme;

const WIDTH: f32 = 240.0;

pub fn view(chat: &Chat) -> Element<'_, Msg> {
    let content: Element<'_, Msg> = match chat.guild.and_then(|g| chat.store.members(g).map(|m| (g, m))) {
        Some((guild_id, members)) => {
            let owner_id = chat.store.guild(guild_id).map(|g| g.owner_id);
            let (online, offline): (Vec<&Member>, Vec<&Member>) =
                members.iter().partition(|m| m.status == Status::Online);
            let mut list = Column::new().spacing(2).padding([16, 8]);
            for (title, group, dimmed) in [("EN LIGNE", &online, false), ("HORS LIGNE", &offline, true)] {
                if group.is_empty() {
                    continue;
                }
                list = list.push(
                    container(
                        text(format!("{title} — {}", group.len())).size(12).font(theme::BOLD).color(theme::c().muted),
                    )
                    .padding([8, 8]),
                );
                for member in group {
                    // Offline: faded, in the colour of their role if they have one, else grey.
                    let color = match chat.store.role_color(guild_id, member.user.id, chat.me.id) {
                        Some(role) if dimmed => theme::role_color(role),
                        _ if dimmed => theme::c().text,
                        _ => chat.name_color(guild_id, member.user.id),
                    };
                    let row = member_row(member, owner_id == Some(member.user.id), dimmed, color);
                    // Roles, exclusion, ban or handing the guild over, as far as we may.
                    list = list.push(if chat.manages(guild_id, member.user.id) {
                        let user_id = member.user.id;
                        button(row)
                            .padding(0)
                            .width(Fill)
                            .style(theme::list_item(false, false))
                            .on_press(Msg::OpenModal(ModalRequest::Member { guild_id, user_id }))
                            .into()
                    } else {
                        row
                    });
                }
            }
            scrollable(list).height(Fill).into()
        }
        None if chat.guild.is_some() => container(text("Chargement…").color(theme::c().muted)).padding(24).into(),
        None => space().into(),
    };
    container(content).width(WIDTH).height(Fill).style(theme::fill(theme::c().sidebar)).into()
}

fn member_row(member: &Member, is_owner: bool, dimmed: bool, color: Color) -> Element<'_, Msg> {
    let name_color = if dimmed { Color { a: 0.45, ..color } } else { color };
    let status_color = if dimmed { theme::c().muted } else { theme::c().green };
    // Presence dot cut into the avatar's bottom-right corner.
    let dot = container(space()).width(12).height(12).style(move |_| container::Style {
        background: Some(status_color.into()),
        border: Border { color: theme::c().sidebar, width: 2.0, radius: 6.0.into() },
        ..container::Style::default()
    });
    let avatar = stack![avatar(&member.user, 32.0), container(dot).align_right(34).align_bottom(34)];
    row![avatar, text(&member.user.username).font(theme::SEMIBOLD).color(name_color)]
        .push(is_owner.then(|| text("★").size(13).color(theme::c().yellow)))
        .spacing(10)
        .align_y(Center)
        .padding([4, 8])
        .into()
}
