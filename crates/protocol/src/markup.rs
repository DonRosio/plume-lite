//! The markup of messages: a subset of Discord's Markdown. The server parses it to find
//! mentions, clients to display messages, so both read a message the same way.
//!
//! - Blocks: code blocks (```` ```rust … ``` ````), quotes (`> ` for one line, `>>> ` for the
//!   rest of the message), headings (`# `, `## `, `### `), list items (`- ` or `* `) and
//!   paragraphs, whose line breaks are kept.
//! - Inline: `**bold**`, `*italic*` or `_italic_`, `__underline__`, `~~strikethrough~~`,
//!   `||spoiler||`, `` `code` ``, web addresses, user mentions (`<@id>`) and `@everyone`.
//!   A backslash makes the next punctuation character literal: `\*not italic\*`.

use crate::Snowflake;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Lines of text; line breaks are kept as `\n` inside the runs.
    Paragraph(Vec<Run>),
    /// `# ` (level 1) to `### ` (level 3).
    Heading(u8, Vec<Run>),
    /// `- ` or `* `.
    Item(Vec<Run>),
    /// Never contains quotes or code blocks.
    Quote(Vec<Block>),
    Code {
        language: Option<String>,
        code: String,
    },
}

/// A piece of inline content and its style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub inline: Inline,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    /// A web address (`http://` or `https://`), shown as written.
    Link(String),
    /// `<@id>`: a user, shown by name.
    User(Snowflake),
    /// `@everyone`. It only notifies anyone when the guild owner writes it
    /// ([`Message::mention_everyone`](crate::model::Message::mention_everyone)).
    Everyone,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// Inline code: nothing else is parsed inside.
    pub code: bool,
    /// Hidden until clicked. The number tells the spoilers of one message apart.
    pub spoiler: Option<u16>,
}

/// Deeper nesting of `**`, `__`, `~~`, `||`, `*` and `_` is shown literally: it keeps the
/// recursion and the cost of a hostile message bounded.
const MAX_DEPTH: usize = 8;

pub fn parse(content: &str) -> Vec<Block> {
    let mut parser = Parser { spoilers: 0 };
    let mut blocks = Vec::new();
    let mut rest = content;
    while let Some((before, language, code, after)) = code_block(rest) {
        parser.lines(before, false, &mut blocks);
        blocks.push(Block::Code { language, code: code.to_owned() });
        rest = after;
    }
    parser.lines(rest, false, &mut blocks);
    blocks
}

/// Who a message notifies: the users it mentions, each once, and whether it says `@everyone`.
pub fn mentions(blocks: &[Block]) -> (Vec<Snowflake>, bool) {
    fn walk(blocks: &[Block], users: &mut Vec<Snowflake>, everyone: &mut bool) {
        for block in blocks {
            let runs = match block {
                Block::Paragraph(runs) | Block::Heading(_, runs) | Block::Item(runs) => runs,
                Block::Quote(inner) => {
                    walk(inner, users, everyone);
                    continue;
                }
                Block::Code { .. } => continue,
            };
            for run in runs {
                match run.inline {
                    Inline::User(id) if !users.contains(&id) => users.push(id),
                    Inline::Everyone => *everyone = true,
                    _ => {}
                }
            }
        }
    }
    let (mut users, mut everyone) = (Vec::new(), false);
    walk(blocks, &mut users, &mut everyone);
    (users, everyone)
}

/// Splits off the first code block: `(text before, language, code, text after)`. The line
/// breaks around the fences belong to the code block.
fn code_block(text: &str) -> Option<(&str, Option<String>, &str, &str)> {
    let start = text.find("```")?;
    let inner_start = start + 3;
    let length = text[inner_start..].find("```")?;
    let inner = &text[inner_start..inner_start + length];
    let (language, code) = match inner.split_once('\n') {
        Some((first, code))
            if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphanumeric() || "+-#._".contains(c)) =>
        {
            (Some(first.to_owned()), code)
        }
        _ => (None, inner),
    };
    let code = code.trim_matches('\n');
    if code.trim().is_empty() {
        return None;
    }
    let before = text[..start].strip_suffix('\n').unwrap_or(&text[..start]);
    let after = &text[inner_start + length + 3..];
    Some((before, language, code, after.strip_prefix('\n').unwrap_or(after)))
}

struct Parser {
    spoilers: u16,
}

impl Parser {
    /// Line-level blocks of a text without code blocks.
    fn lines(&mut self, text: &str, quoted: bool, blocks: &mut Vec<Block>) {
        if text.is_empty() {
            return;
        }
        let mut paragraph: Vec<&str> = Vec::new();
        let mut quote: Vec<&str> = Vec::new();
        let mut lines = text.split('\n');
        while let Some(line) = lines.next() {
            if !quoted {
                if let Some(first) = line.strip_prefix(">>> ") {
                    // The rest of the message is quoted.
                    self.paragraph(&mut paragraph, blocks);
                    quote.push(first);
                    quote.extend(lines.by_ref());
                    break;
                }
                if let Some(quoted_line) = line.strip_prefix("> ").or((line == ">").then_some("")) {
                    self.paragraph(&mut paragraph, blocks);
                    quote.push(quoted_line);
                    continue;
                }
                self.quote(&mut quote, blocks);
            }
            let heading = ["# ", "## ", "### "].iter().position(|prefix| line.starts_with(prefix));
            if let Some(level) = heading {
                self.paragraph(&mut paragraph, blocks);
                blocks.push(Block::Heading(level as u8 + 1, self.inline(&line[level + 2..])));
            } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                self.paragraph(&mut paragraph, blocks);
                blocks.push(Block::Item(self.inline(item)));
            } else {
                paragraph.push(line);
            }
        }
        self.paragraph(&mut paragraph, blocks);
        self.quote(&mut quote, blocks);
    }

    fn paragraph(&mut self, lines: &mut Vec<&str>, blocks: &mut Vec<Block>) {
        if !lines.is_empty() {
            blocks.push(Block::Paragraph(self.inline(&lines.join("\n"))));
            lines.clear();
        }
    }

    fn quote(&mut self, lines: &mut Vec<&str>, blocks: &mut Vec<Block>) {
        if !lines.is_empty() {
            let mut inner = Vec::new();
            self.lines(&lines.join("\n"), true, &mut inner);
            blocks.push(Block::Quote(inner));
            lines.clear();
        }
    }

    fn inline(&mut self, text: &str) -> Vec<Run> {
        let mut runs = Vec::new();
        self.inline_into(text, Style::default(), 0, &mut runs);
        runs
    }

    fn inline_into(&mut self, text: &str, style: Style, depth: usize, runs: &mut Vec<Run>) {
        let mut plain = String::new();
        let flush = |plain: &mut String, runs: &mut Vec<Run>| {
            if !plain.is_empty() {
                runs.push(Run { inline: Inline::Text(std::mem::take(plain)), style });
            }
        };
        let mut i = 0;
        'scan: while i < text.len() {
            let rest = &text[i..];
            let after_boundary = text[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric());

            if let Some(escaped) = rest.strip_prefix('\\').and_then(|r| r.chars().next())
                && escaped.is_ascii_punctuation()
            {
                plain.push(escaped);
                i += 1 + escaped.len_utf8();
                continue;
            }
            if rest.starts_with('`') {
                let fence = if rest.starts_with("``") { "``" } else { "`" };
                if let Some(length) = rest[fence.len()..].find(fence).filter(|&n| n > 0) {
                    let code = &rest[fence.len()..fence.len() + length];
                    flush(&mut plain, runs);
                    let code_style = Style { code: true, ..style };
                    runs.push(Run { inline: Inline::Text(code.to_owned()), style: code_style });
                    i += 2 * fence.len() + length;
                    continue;
                }
            }
            if after_boundary && (rest.starts_with("https://") || rest.starts_with("http://")) {
                let length = link_length(rest);
                if length > "https://".len() {
                    flush(&mut plain, runs);
                    runs.push(Run { inline: Inline::Link(rest[..length].to_owned()), style });
                    i += length;
                    continue;
                }
            }
            if let Some((id, length)) = user_mention(rest) {
                flush(&mut plain, runs);
                runs.push(Run { inline: Inline::User(id), style });
                i += length;
                continue;
            }
            if after_boundary
                && let Some(after) = rest.strip_prefix("@everyone")
                && after.chars().next().is_none_or(|c| !is_username_char(c))
            {
                flush(&mut plain, runs);
                runs.push(Run { inline: Inline::Everyone, style });
                i += "@everyone".len();
                continue;
            }
            if depth < MAX_DEPTH {
                for marker in ["**", "__", "~~", "||", "*", "_"] {
                    if !rest.starts_with(marker) || (marker == "_" && !after_boundary) {
                        continue;
                    }
                    let Some(length) = closing(&rest[marker.len()..], marker) else { continue };
                    let inner = &rest[marker.len()..marker.len() + length];
                    let mut inner_style = style;
                    match marker {
                        "**" => inner_style.bold = true,
                        "__" => inner_style.underline = true,
                        "~~" => inner_style.strikethrough = true,
                        "||" => {
                            inner_style.spoiler = Some(self.spoilers);
                            self.spoilers = self.spoilers.saturating_add(1);
                        }
                        _ => inner_style.italic = true,
                    }
                    flush(&mut plain, runs);
                    self.inline_into(inner, inner_style, depth + 1, runs);
                    i += 2 * marker.len() + length;
                    continue 'scan;
                }
            }
            let c = rest.chars().next().unwrap_or_default();
            plain.push(c);
            i += c.len_utf8().max(1);
        }
        flush(&mut plain, runs);
    }
}

/// Where the closing `marker` is in `text` (which starts right after the opening one), if
/// the content in between is not empty.
fn closing(text: &str, marker: &str) -> Option<usize> {
    let first = text.chars().next()?;
    match marker {
        // `*italic*`: not `* spaced*`, and `**bold**` inside is skipped.
        "*" => {
            if first.is_whitespace() {
                return None;
            }
            let bytes = text.as_bytes();
            let mut p = first.len_utf8();
            while p < bytes.len() {
                match bytes[p] {
                    b'\\' => p += 2,
                    b'*' if bytes.get(p + 1) == Some(&b'*') => p += 2,
                    b'*' if !text[..p].ends_with(char::is_whitespace) => return Some(p),
                    _ => p += 1,
                }
            }
            None
        }
        // `_italic_`: not inside words, so `snake_case_names` stay as they are.
        "_" => {
            let mut from = first.len_utf8();
            while let Some(found) = text.get(from..)?.find('_') {
                let p = from + found;
                if text[p + 1..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_') {
                    return Some(p);
                }
                from = p + 1;
            }
            None
        }
        // The earliest closing marker not followed by one more of its character, so that
        // `***both***` is bold around italic.
        _ => {
            let repeated = marker.chars().next()?;
            let mut from = first.len_utf8();
            while let Some(found) = text.get(from..)?.find(marker) {
                let p = from + found;
                if text[p + marker.len()..].starts_with(repeated) {
                    from = p + 1;
                    continue;
                }
                return Some(p);
            }
            None
        }
    }
}

/// Length of the web address at the start of `text`: up to a space or `<>`, without the
/// punctuation that usually ends a sentence, nor a `)` that closes text around it.
fn link_length(text: &str) -> usize {
    let mut end = text.find(|c: char| c.is_whitespace() || c == '<' || c == '>').unwrap_or(text.len());
    loop {
        let link = &text[..end];
        let Some(last) = link.chars().next_back() else { return 0 };
        let unbalanced = last == ')' && link.matches('(').count() < link.matches(')').count();
        if ".,:;!?'\"*_~|".contains(last) || unbalanced {
            end -= last.len_utf8();
        } else {
            return end;
        }
    }
}

/// `<@123>` (or `<@!123>`) at the start of `text`: the user and the length of the mention.
fn user_mention(text: &str) -> Option<(Snowflake, usize)> {
    let digits_start = if text.starts_with("<@!") { 3 } else { 2 };
    if !text.starts_with("<@") {
        return None;
    }
    let digits = text[digits_start..].find(|c: char| !c.is_ascii_digit())?;
    if digits == 0 || !text[digits_start + digits..].starts_with('>') {
        return None;
    }
    let id = text[digits_start..digits_start + digits].parse::<i64>().ok()?;
    Some((Snowflake(id), digits_start + digits + 1))
}

/// The characters of a username (see [`crate::validate::username`]).
pub fn is_username_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str, style: Style) -> Run {
        Run { inline: Inline::Text(text.to_owned()), style }
    }

    const PLAIN: Style =
        Style { bold: false, italic: false, underline: false, strikethrough: false, code: false, spoiler: None };
    const BOLD: Style = Style { bold: true, ..PLAIN };
    const ITALIC: Style = Style { italic: true, ..PLAIN };

    fn paragraph(content: &str) -> Vec<Run> {
        match parse(content).as_slice() {
            [Block::Paragraph(runs)] => runs.clone(),
            other => panic!("not one paragraph: {other:?}"),
        }
    }

    #[test]
    fn inline_styles() {
        assert_eq!(paragraph("a **b** c"), vec![text("a ", PLAIN), text("b", BOLD), text(" c", PLAIN)]);
        assert_eq!(paragraph("*i* _j_"), vec![text("i", ITALIC), text(" ", PLAIN), text("j", ITALIC)]);
        assert_eq!(paragraph("***x***"), vec![text("x", Style { bold: true, italic: true, ..PLAIN })]);
        assert_eq!(
            paragraph("__u__ ~~s~~"),
            vec![
                text("u", Style { underline: true, ..PLAIN }),
                text(" ", PLAIN),
                text("s", Style { strikethrough: true, ..PLAIN })
            ]
        );
        assert_eq!(
            paragraph("*a **b** c*"),
            vec![text("a ", ITALIC), text("b", Style { bold: true, italic: true, ..PLAIN }), text(" c", ITALIC)]
        );
    }

    #[test]
    fn what_is_not_markup_stays_as_written() {
        for literal in ["snake_case_name", "2 * 3 * 4", "a ** b", "**", "**unclosed", "a * b*", "``", "<@>"] {
            assert_eq!(paragraph(literal), vec![text(literal, PLAIN)], "{literal}");
        }
        assert_eq!(paragraph(r"\*pas italique\*"), vec![text("*pas italique*", PLAIN)]);
        assert_eq!(paragraph("`**pas gras**`"), vec![text("**pas gras**", Style { code: true, ..PLAIN })]);
    }

    #[test]
    fn spoilers_are_numbered() {
        let runs = paragraph("||a|| et ||b||");
        assert_eq!(runs[0].style.spoiler, Some(0));
        assert_eq!(runs[2].style.spoiler, Some(1));
    }

    #[test]
    fn links_mentions_and_everyone() {
        let runs = paragraph("voir https://exemple.fr/a_b_c (https://x.fr/p). <@42> @everyone, pas@everyone");
        assert_eq!(runs[1].inline, Inline::Link("https://exemple.fr/a_b_c".into()));
        assert_eq!(runs[3].inline, Inline::Link("https://x.fr/p".into()));
        assert_eq!(runs[5].inline, Inline::User(Snowflake(42)));
        assert_eq!(runs[7].inline, Inline::Everyone);
        assert_eq!(runs[8], text(", pas@everyone", PLAIN));
        let blocks = parse("<@1> <@!2> <@1> `<@3>` @everyone\n```\n<@4>\n```");
        assert_eq!(mentions(&blocks), (vec![Snowflake(1), Snowflake(2)], true));
        assert_eq!(mentions(&parse("@everyones <@x>")), (vec![], false));
    }

    #[test]
    fn blocks() {
        let blocks = parse("# Titre\ntexte\nsuite\n- un\n> cité\n> encore\nfin\n```rust\nfn main() {}\n```\naprès");
        assert_eq!(
            blocks,
            vec![
                Block::Heading(1, vec![text("Titre", PLAIN)]),
                Block::Paragraph(vec![text("texte\nsuite", PLAIN)]),
                Block::Item(vec![text("un", PLAIN)]),
                Block::Quote(vec![Block::Paragraph(vec![text("cité\nencore", PLAIN)])]),
                Block::Paragraph(vec![text("fin", PLAIN)]),
                Block::Code { language: Some("rust".into()), code: "fn main() {}".into() },
                Block::Paragraph(vec![text("après", PLAIN)]),
            ]
        );
        assert_eq!(
            parse(">>> tout\n# le reste"),
            vec![Block::Quote(vec![
                Block::Paragraph(vec![text("tout", PLAIN)]),
                Block::Heading(1, vec![text("le reste", PLAIN)]),
            ])]
        );
        assert_eq!(
            parse("a ```b``` c"),
            vec![
                Block::Paragraph(vec![text("a ", PLAIN)]),
                Block::Code { language: None, code: "b".into() },
                Block::Paragraph(vec![text(" c", PLAIN)]),
            ]
        );
    }

    #[test]
    fn hostile_input_stays_cheap() {
        let nested = "**__~~||*".repeat(500);
        let _ = parse(&nested);
        let _ = parse(&"*".repeat(4000));
        let _ = parse(&"`".repeat(4000));
        let _ = parse(&"_a".repeat(2000));
    }
}
