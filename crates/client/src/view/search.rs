//! Search results, in place of the member list.

use iced::widget::{Column, button, column, container, row, scrollable, space, text};
use iced::{Center, Element, Fill};

use super::markup::{self, Context};
use super::message_time;
use crate::app::{Chat, Msg, SearchPanel};
use crate::theme;

const WIDTH: u32 = 360;

pub fn view<'a>(chat: &'a Chat, panel: &'a SearchPanel) -> Element<'a, Msg> {
    let c = theme::c();
    let count = match (panel.loading && panel.results.is_empty(), panel.total) {
        (true, _) => "Recherche…".to_owned(),
        (false, 0) => "Aucun résultat".to_owned(),
        (false, 1) => "1 résultat".to_owned(),
        (false, n) => format!("{n} résultats"),
    };
    let header = row![
        text(count).size(14).font(theme::BOLD).color(c.bright).width(Fill),
        button(text("✕").size(14)).padding([2, 8]).style(theme::ghost).on_press(Msg::CloseSearch),
    ]
    .align_y(Center)
    .padding([12, 12]);

    let mut list = Column::new().spacing(8).padding([0, 8]);
    for (message, blocks) in &panel.results {
        let channel = chat.store.channel(message.channel_id).map_or("…", |ch| ch.name.as_str());
        let ctx = Context {
            chat,
            guild_id: message.guild_id,
            message_id: message.id,
            everyone: message.mention_everyone,
            color: None,
        };
        let line = row![
            text(format!("#{channel}")).size(12).font(theme::SEMIBOLD).color(c.muted),
            text(&message.author.username).size(13).font(theme::SEMIBOLD).color(theme::user_color(message.author.id)),
            text(message_time(message.created_at)).size(11).color(c.muted),
        ]
        .spacing(6)
        .align_y(Center);
        let open = button(text("Aller au salon").size(12))
            .padding([2, 8])
            .style(theme::message_action(false))
            .on_press(Msg::OpenSearchResult(message.channel_id));
        list = list.push(
            container(column![line, markup::view(ctx, blocks, None), row![space::horizontal(), open]].spacing(4))
                .padding(10)
                .width(Fill)
                .style(theme::rounded_fill(c.chat, 6.0)),
        );
    }
    if let Some(error) = &panel.error {
        list = list.push(text(error).size(13).color(c.red));
    }
    if (panel.results.len() as u64) < panel.total {
        list = list.push(
            container(
                button(text(if panel.loading { "Chargement…" } else { "Plus de résultats" }).size(13))
                    .style(theme::ghost)
                    .on_press_maybe((!panel.loading).then_some(Msg::SearchMore)),
            )
            .center_x(Fill)
            .padding(8),
        );
    }
    container(column![header, scrollable(list.padding([0, 8]).push(space().height(12))).height(Fill)])
        .width(WIDTH)
        .height(Fill)
        .style(theme::fill(c.sidebar))
        .into()
}
