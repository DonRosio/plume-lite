//! Displays the markup of messages (`plume_lite_protocol::markup`): emphasis, code, quotes,
//! headings, lists, spoilers, links and mentions.

use std::borrow::Cow;

use iced::widget::text::{Span, Wrapping};
use iced::widget::{Column, container, rich_text, row, space, span, stack, text};
use iced::{Border, Color, Element, Fill, Font};
use plume_lite_protocol::Snowflake;
use plume_lite_protocol::markup::{self, Block, Inline, Run};

use crate::app::{Chat, Msg};
use crate::theme;

/// What a click on a part of a message does.
#[derive(Debug, Clone)]
pub enum Link {
    Url(String),
    /// Reveals the spoiler with this number.
    Spoiler(u16),
}

/// What displaying the markup of one message needs.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub chat: &'a Chat,
    pub guild_id: Snowflake,
    pub message_id: Snowflake,
    /// `@everyone` notifies: written by someone allowed to.
    pub everyone: bool,
    /// Text colour, if not the usual one (a message being sent, or that failed).
    pub color: Option<Color>,
}

/// The blocks of a message. `suffix` (" (modifié)") follows the last line of text.
pub fn view<'a>(ctx: Context<'a>, blocks: &'a [Block], suffix: Option<Span<'a, Link>>) -> Element<'a, Msg> {
    let mut suffix = suffix;
    let mut column = Column::new().spacing(4).width(Fill);
    let last = blocks.len().saturating_sub(1);
    for (i, block) in blocks.iter().enumerate() {
        column = column.push(block_view(ctx, block, if i == last { suffix.take() } else { None }));
    }
    column.into()
}

fn block_view<'a>(ctx: Context<'a>, block: &'a Block, suffix: Option<Span<'a, Link>>) -> Element<'a, Msg> {
    let c = theme::c();
    match block {
        Block::Paragraph(runs) => paragraph(ctx, runs, None, suffix),
        Block::Heading(level, runs) => {
            let size = match level {
                1 => 22.0,
                2 => 19.0,
                _ => 17.0,
            };
            paragraph(ctx, runs, Some(size), suffix)
        }
        Block::Item(runs) => {
            row![text("•").color(ctx.color.unwrap_or(c.muted)), paragraph(ctx, runs, None, suffix)].spacing(8).into()
        }
        // The bar is a layer over the margin: a `Fill` height inside a row would make the
        // message as tall as the whole list.
        Block::Quote(inner) => stack![
            container(view(ctx, inner, suffix)).padding(iced::Padding { left: 14.0, ..iced::Padding::ZERO }),
            container(space()).width(4).height(Fill).style(theme::rounded_fill(c.quote_bar, 2.0)),
        ]
        .into(),
        Block::Code { code, .. } => {
            let code = container(text(code.as_str()).font(Font::MONOSPACE).size(14).color(ctx.color.unwrap_or(c.text)))
                .padding(8)
                .width(Fill)
                .style(theme::rounded_fill(c.code, 4.0));
            match suffix {
                Some(suffix) => Column::new().push(code).push(rich_text([suffix]).on_link_click(on_link(ctx))).into(),
                None => code.into(),
            }
        }
    }
}

/// Runs of text; headings are bold and larger.
fn paragraph<'a>(
    ctx: Context<'a>,
    runs: &'a [Run],
    heading: Option<f32>,
    suffix: Option<Span<'a, Link>>,
) -> Element<'a, Msg> {
    let mut spans: Vec<Span<'a, Link>> = runs
        .iter()
        .map(|run| {
            let span = run_span(ctx, run, true);
            if heading.is_some() {
                span.font(theme::message_font(true, run.style.italic, run.style.code))
            } else {
                span
            }
        })
        .collect();
    join_words(&mut spans);
    spans.extend(suffix);
    let mut paragraph =
        rich_text(spans).width(Fill).color(ctx.color.unwrap_or(theme::c().text)).on_link_click(on_link(ctx));
    if let Some(size) = heading {
        paragraph = paragraph.size(size);
    }
    paragraph.into()
}

fn on_link(ctx: Context<'_>) -> impl Fn(Link) -> Msg + use<> {
    let message_id = ctx.message_id;
    move |link| match link {
        Link::Url(url) => Msg::OpenUrl(url),
        Link::Spoiler(n) => Msg::RevealSpoiler(message_id, n),
    }
}

/// One run with its style. Without `interactive`, links and spoilers do nothing (excerpts).
fn run_span<'b>(ctx: Context<'_>, run: &'b Run, interactive: bool) -> Span<'b, Link> {
    let c = theme::c();
    let style = run.style;
    let pill = |span: Span<'b, Link>| {
        span.color(c.link).background(c.mention_pill).border(Border { radius: 3.0.into(), ..Border::default() })
    };
    let mut span: Span<'b, Link> = match &run.inline {
        Inline::Text(text) => span(text.as_str()),
        Inline::Link(url) => {
            let link = span(url.as_str()).color(c.link).underline(true);
            if interactive { link.link(Link::Url(url.clone())) } else { link }
        }
        Inline::User(id) => {
            let name = ctx.chat.store.user_name(ctx.guild_id, *id).unwrap_or_else(|| "utilisateur inconnu".to_owned());
            pill(span(format!("@{name}")))
        }
        Inline::Everyone if ctx.everyone => pill(span("@everyone")),
        Inline::Everyone => span("@everyone"),
    };
    span = span.font(theme::message_font(style.bold, style.italic, style.code));
    if style.underline {
        span = span.underline(true);
    }
    if style.strikethrough {
        span = span.strikethrough(true);
    }
    if style.code {
        span = span.background(c.code).border(Border { radius: 3.0.into(), ..Border::default() });
    }
    if let Some(n) = style.spoiler {
        let revealed = interactive && ctx.chat.revealed.contains(&(ctx.message_id, n));
        span = if revealed {
            span.background(c.code)
        } else {
            // Hidden: the text takes the colour of its background.
            let hidden = span.color(c.spoiler).background(c.spoiler);
            if interactive { hidden.link(Link::Spoiler(n)) } else { hidden }
        };
    }
    span
}

/// cosmic-text (0.15) shapes a word made of ASCII only with the font of its first letter, so a
/// style that starts or ends inside a word (`l'*italique*`, `**gras**,`) would be lost. An
/// invisible word joiner (U+2060, no line break) sends such words down its slower path, which
/// follows every change of font. Only the displayed text changes: copying uses the message.
fn join_words(spans: &mut [Span<'_, Link>]) {
    for i in 1..spans.len() {
        let (before, after) = spans.split_at_mut(i);
        let (previous, next) = (&before[i - 1], &mut after[0]);
        let inside_word = previous.text.ends_with(|c: char| !c.is_whitespace())
            && next.text.starts_with(|c: char| !c.is_whitespace());
        if inside_word && previous.font != next.font {
            next.text = Cow::Owned(format!("\u{2060}{}", next.text));
        }
    }
}

/// The beginning of an answered message, on one line, without line breaks nor blocks.
pub fn excerpt<'a>(ctx: Context<'a>, content: &str) -> Element<'a, Msg> {
    fn collect(ctx: Context<'_>, blocks: &[Block], spans: &mut Vec<Span<'static, Link>>) {
        for block in blocks {
            if !spans.is_empty() {
                spans.push(span(" "));
            }
            match block {
                Block::Paragraph(runs) | Block::Heading(_, runs) | Block::Item(runs) => {
                    for run in runs {
                        let mut span = run_span(ctx, run, false).to_static();
                        // Line breaks would make the excerpt taller.
                        if let Inline::Text(text) = &run.inline
                            && text.contains('\n')
                        {
                            span.text = Cow::Owned(text.replace('\n', " "));
                        }
                        spans.push(span);
                    }
                }
                Block::Quote(inner) => collect(ctx, inner, spans),
                Block::Code { code, .. } => {
                    spans.push(span(code.replace('\n', " ")).font(Font::MONOSPACE).background(theme::c().code));
                }
            }
        }
    }
    let mut spans = Vec::new();
    collect(ctx, &markup::parse(content), &mut spans);
    join_words(&mut spans);
    container(rich_text(spans).size(13).color(theme::c().muted).wrapping(Wrapping::None).on_link_click(on_link(ctx)))
        .width(Fill)
        .clip(true)
        .into()
}
