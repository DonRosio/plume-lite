//! Display order of a guild's channels, shared by the server (which stores it as `position`)
//! and the client (which draws it).
//!
//! Channels without a category come first, then each category in order; inside each, text
//! channels come before voice channels. After every change the server renumbers the
//! channels of the guild in that order (0, 1, …), and the categories among themselves, so
//! that clients sorting by position alone (before 0.6) still list them in the same order.

use crate::Snowflake;
use crate::model::{Category, Channel, ChannelKind};

/// The channels listed under one heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section<'a> {
    /// `None`: the channels above every category.
    pub category: Option<&'a Category>,
    pub channels: Vec<&'a Channel>,
}

/// The guild's channels in display order. The first section, without a category, is always
/// there (possibly empty); a channel whose category is unknown is listed in it.
pub fn sections<'a>(
    categories: impl IntoIterator<Item = &'a Category>,
    channels: impl IntoIterator<Item = &'a Channel>,
) -> Vec<Section<'a>> {
    let mut categories: Vec<&Category> = categories.into_iter().collect();
    categories.sort_by_key(|category| (category.position, category.id));
    let mut sections = vec![Section { category: None, channels: Vec::new() }];
    sections.extend(categories.into_iter().map(|category| Section { category: Some(category), channels: Vec::new() }));
    for channel in channels {
        let index = channel
            .category_id
            .and_then(|id| sections.iter().position(|s| s.category.is_some_and(|c| c.id == id)))
            .unwrap_or(0);
        sections[index].channels.push(channel);
    }
    for section in &mut sections {
        section.channels.sort_by_key(|channel| (rank(channel.kind), channel.position, channel.id));
    }
    sections
}

/// Numbers categories and channels in display order (0, 1, …).
pub fn renumber(categories: &mut [Category], channels: &mut [Channel]) {
    categories.sort_by_key(|category| (category.position, category.id));
    for (position, category) in (0..).zip(categories.iter_mut()) {
        category.position = position;
    }
    let order: Vec<Snowflake> =
        sections(categories.iter(), channels.iter()).iter().flat_map(|s| s.channels.iter().map(|c| c.id)).collect();
    for (position, id) in (0..).zip(order) {
        if let Some(channel) = channels.iter_mut().find(|channel| channel.id == id) {
            channel.position = position;
        }
    }
}

/// Puts a channel under `category` (one of `categories`, or `None`) at `index` among the
/// channels of its kind there (last when `None` or past the end), then renumbers.
pub fn move_channel(
    categories: &mut [Category],
    channels: &mut [Channel],
    id: Snowflake,
    category: Option<Snowflake>,
    index: Option<usize>,
) {
    renumber(categories, channels);
    let category = category.filter(|c| categories.iter().any(|known| known.id == *c));
    let Some(moved) = channels.iter().position(|channel| channel.id == id) else { return };
    let kind = rank(channels[moved].kind);
    // Spread the others out, then slip the channel in just before its new neighbour.
    for channel in channels.iter_mut() {
        channel.position *= 2;
    }
    let mut siblings: Vec<i32> = channels
        .iter()
        .filter(|c| c.id != id && section_of(categories, c) == category && rank(c.kind) == kind)
        .map(|c| c.position)
        .collect();
    siblings.sort_unstable();
    channels[moved].category_id = category;
    channels[moved].position = match index.and_then(|i| siblings.get(i)) {
        Some(next) => next - 1,
        None => siblings.last().map_or(i32::MAX, |last| last + 1),
    };
    renumber(categories, channels);
}

/// Puts a category at `index` among the categories (last when past the end), then renumbers.
pub fn move_category(categories: &mut [Category], channels: &mut [Channel], id: Snowflake, index: usize) {
    renumber(categories, channels);
    for category in categories.iter_mut() {
        category.position *= 2;
    }
    let mut others: Vec<i32> = categories.iter().filter(|c| c.id != id).map(|c| c.position).collect();
    others.sort_unstable();
    let position = others.get(index).map_or(i32::MAX, |next| next - 1);
    if let Some(category) = categories.iter_mut().find(|c| c.id == id) {
        category.position = position;
    }
    renumber(categories, channels);
}

/// Text channels first, then voice channels.
fn rank(kind: ChannelKind) -> u8 {
    match kind {
        ChannelKind::Text => 0,
        ChannelKind::Voice => 1,
        ChannelKind::Unknown => 2,
    }
}

/// The category a channel is listed under (`None` if its category is unknown).
fn section_of(categories: &[Category], channel: &Channel) -> Option<Snowflake> {
    channel.category_id.filter(|id| categories.iter().any(|c| c.id == *id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUILD: Snowflake = Snowflake(1);

    fn channel(id: i64, kind: ChannelKind, position: i32, category: Option<i64>) -> Channel {
        Channel {
            id: Snowflake(id),
            guild_id: GUILD,
            name: format!("c{id}"),
            kind,
            position,
            category_id: category.map(Snowflake),
            overwrites: vec![],
            last_message_id: None,
        }
    }

    fn category(id: i64, position: i32) -> Category {
        Category { id: Snowflake(id), guild_id: GUILD, name: format!("k{id}"), position }
    }

    /// Display order as `(category, [channels])`, `0` for "no category".
    fn shown(categories: &[Category], channels: &[Channel]) -> Vec<(i64, Vec<i64>)> {
        sections(categories, channels)
            .iter()
            .map(|s| (s.category.map_or(0, |c| c.id.0), s.channels.iter().map(|c| c.id.0).collect()))
            .collect()
    }

    fn positions(channels: &[Channel]) -> Vec<(i64, i32)> {
        let mut list: Vec<(i64, i32)> = channels.iter().map(|c| (c.id.0, c.position)).collect();
        list.sort_unstable();
        list
    }

    #[test]
    fn sections_put_text_before_voice_and_tolerate_unknown_categories() {
        use ChannelKind::{Text, Voice};
        let categories = [category(200, 1), category(100, 0)];
        let channels = [
            channel(1, Voice, 0, None),
            channel(2, Text, 5, None),
            channel(3, Text, 1, Some(200)),
            channel(4, Text, 0, Some(999)), // its category is not known (yet)
            channel(5, Voice, 0, Some(100)),
            channel(6, Text, 9, Some(100)),
        ];
        assert_eq!(shown(&categories, &channels), [(0, vec![4, 2, 1]), (100, vec![6, 5]), (200, vec![3])]);
        assert_eq!(shown(&[], &[]), [(0, vec![])]);
    }

    #[test]
    fn renumbering_follows_the_display_order() {
        use ChannelKind::{Text, Voice};
        let mut categories = vec![category(100, 7), category(200, 3)];
        let mut channels =
            vec![channel(1, Voice, 0, None), channel(2, Text, 4, Some(100)), channel(3, Text, 9, Some(200))];
        renumber(&mut categories, &mut channels);
        assert_eq!(categories.iter().map(|c| (c.id.0, c.position)).collect::<Vec<_>>(), [(200, 0), (100, 1)]);
        assert_eq!(positions(&channels), [(1, 0), (2, 2), (3, 1)]);
    }

    #[test]
    fn channels_move_within_their_kind_and_between_categories() {
        use ChannelKind::{Text, Voice};
        let mut categories = vec![category(100, 0)];
        let mut channels = vec![
            channel(1, Text, 0, None),
            channel(2, Text, 1, None),
            channel(3, Text, 2, None),
            channel(4, Voice, 3, None),
            channel(5, Text, 4, Some(100)),
        ];
        // Up one rank.
        move_channel(&mut categories, &mut channels, Snowflake(3), None, Some(1));
        assert_eq!(shown(&categories, &channels), [(0, vec![1, 3, 2, 4]), (100, vec![5])]);
        // First of its kind, even far past the voice channel's index.
        move_channel(&mut categories, &mut channels, Snowflake(2), None, Some(0));
        assert_eq!(shown(&categories, &channels), [(0, vec![2, 1, 3, 4]), (100, vec![5])]);
        // Into a category: last there, by default.
        move_channel(&mut categories, &mut channels, Snowflake(1), Some(Snowflake(100)), None);
        assert_eq!(shown(&categories, &channels), [(0, vec![2, 3, 4]), (100, vec![5, 1])]);
        // A voice channel cannot go above text ones: it is first among voice channels.
        move_channel(&mut categories, &mut channels, Snowflake(4), Some(Snowflake(100)), Some(0));
        assert_eq!(shown(&categories, &channels), [(0, vec![2, 3]), (100, vec![5, 1, 4])]);
        // Past the end: last. Unknown category: none.
        move_channel(&mut categories, &mut channels, Snowflake(5), Some(Snowflake(100)), Some(99));
        move_channel(&mut categories, &mut channels, Snowflake(3), Some(Snowflake(999)), None);
        assert_eq!(shown(&categories, &channels), [(0, vec![2, 3]), (100, vec![1, 5, 4])]);
        assert_eq!(positions(&channels), [(1, 2), (2, 0), (3, 1), (4, 4), (5, 3)]);
    }

    #[test]
    fn categories_move_and_carry_their_channels() {
        use ChannelKind::Text;
        let mut categories = vec![category(100, 0), category(200, 1), category(300, 2)];
        let mut channels =
            vec![channel(1, Text, 0, Some(100)), channel(2, Text, 1, Some(200)), channel(3, Text, 2, Some(300))];
        move_category(&mut categories, &mut channels, Snowflake(300), 0);
        assert_eq!(shown(&categories, &channels), [(0, vec![]), (300, vec![3]), (100, vec![1]), (200, vec![2])]);
        assert_eq!(positions(&channels), [(1, 1), (2, 2), (3, 0)]);
        move_category(&mut categories, &mut channels, Snowflake(300), 9);
        assert_eq!(shown(&categories, &channels), [(0, vec![]), (100, vec![1]), (200, vec![2]), (300, vec![3])]);
    }
}
