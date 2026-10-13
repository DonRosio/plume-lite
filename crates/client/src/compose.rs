//! Writing messages: the composer shows mentions as `@name`, the protocol carries them as
//! `<@id>` (see `plume_lite_protocol::markup`). Pure functions, so they are easy to test.

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::markup::is_username_char;
use plume_lite_protocol::validate::USERNAME_MAX;

/// The name being typed right before the cursor (`cursor` is a byte offset in `line`):
/// what follows `@`, possibly empty.
pub fn mention_query(line: &str, cursor: usize) -> Option<&str> {
    let before = line.get(..cursor)?;
    let at = before.rfind('@')?;
    let query = &before[at + 1..];
    let after_boundary = before[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
    (after_boundary && query.len() <= USERNAME_MAX && query.chars().all(is_username_char)).then_some(query)
}

/// `@name` → `<@id>` for the users `lookup` knows, outside code.
pub fn encode_mentions(text: &str, lookup: impl Fn(&str) -> Option<Snowflake>) -> String {
    outside_code(text, |plain, out| {
        let mut rest = plain;
        while let Some(at) = rest.find('@') {
            let (before, after) = rest.split_at(at);
            out.push_str(before);
            let after_boundary = out.chars().next_back().is_none_or(|c| !c.is_alphanumeric());
            let name_len = after[1..].find(|c: char| !is_username_char(c)).unwrap_or(after.len() - 1);
            // A sentence may end right after the name: "merci @alice."
            let name = after[1..1 + name_len].trim_end_matches('.');
            match lookup(name).filter(|_| after_boundary && !name.is_empty()) {
                Some(id) => {
                    out.push_str(&format!("<@{id}>"));
                    rest = &after[1 + name.len()..];
                }
                None => {
                    out.push('@');
                    rest = &after[1..];
                }
            }
        }
        out.push_str(rest);
    })
}

/// `<@id>` → `@name` for the users `name_of` knows, outside code: to edit a message.
pub fn decode_mentions(content: &str, name_of: impl Fn(Snowflake) -> Option<String>) -> String {
    outside_code(content, |plain, out| {
        let mut rest = plain;
        while let Some(start) = rest.find("<@") {
            out.push_str(&rest[..start]);
            let tail = &rest[start..];
            let digits_start = if tail.starts_with("<@!") { 3 } else { 2 };
            let digits = tail[digits_start..].find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
            let id = tail[digits_start..digits_start + digits].parse::<i64>().ok().map(Snowflake);
            let closed = tail[digits_start + digits..].starts_with('>');
            match id.filter(|_| closed).and_then(&name_of) {
                Some(name) => {
                    out.push('@');
                    out.push_str(&name);
                    rest = &tail[digits_start + digits + 1..];
                }
                None => {
                    out.push_str("<@");
                    rest = &tail[2..];
                }
            }
        }
        out.push_str(rest);
    })
}

/// Rewrites the parts of `text` that are not code (between backticks) with `rewrite`.
fn outside_code(text: &str, mut rewrite: impl FnMut(&str, &mut String)) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, block) in text.split("```").enumerate() {
        if i > 0 {
            out.push_str("```");
        }
        if i % 2 == 1 {
            out.push_str(block);
            continue;
        }
        for (j, span) in block.split('`').enumerate() {
            if j > 0 {
                out.push('`');
            }
            if j % 2 == 1 {
                out.push_str(span);
            } else {
                rewrite(span, &mut out);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(name: &str) -> Option<Snowflake> {
        match name.to_lowercase().as_str() {
            "alice" => Some(Snowflake(1)),
            "bob.l" => Some(Snowflake(2)),
            _ => None,
        }
    }

    fn name_of(id: Snowflake) -> Option<String> {
        match id.0 {
            1 => Some("alice".into()),
            2 => Some("bob.l".into()),
            _ => None,
        }
    }

    #[test]
    fn the_name_being_typed() {
        assert_eq!(mention_query("salut @al", 9), Some("al"));
        assert_eq!(mention_query("@", 1), Some(""));
        assert_eq!(mention_query("salut @al", 7), Some(""));
        assert_eq!(mention_query("mail@al", 7), None, "not inside a word");
        assert_eq!(mention_query("@alice bonjour", 14), None, "a space ends it");
        assert_eq!(mention_query("é @a", "é @a".len()), Some("a"));
    }

    #[test]
    fn mentions_are_encoded_outside_code() {
        assert_eq!(encode_mentions("@Alice et @bob.l.", lookup), "<@1> et <@2>.");
        assert_eq!(encode_mentions("merci @alice.", lookup), "merci <@1>.");
        assert_eq!(encode_mentions("@inconnu mail@alice @", lookup), "@inconnu mail@alice @");
        assert_eq!(encode_mentions("`@alice` ```\n@alice\n``` @alice", lookup), "`@alice` ```\n@alice\n``` <@1>");
        assert_eq!(encode_mentions("@everyone", lookup), "@everyone");
    }

    #[test]
    fn mentions_are_decoded_for_editing() {
        assert_eq!(decode_mentions("<@1> et <@!2>, <@3> <@ `<@1>`", name_of), "@alice et @bob.l, <@3> <@ `<@1>`");
        let content = "salut <@1> !";
        assert_eq!(encode_mentions(&decode_mentions(content, name_of), lookup), content);
    }
}

/// A search as typed: its words, and the `de:name` and `dans:#channel` filters.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SearchInput {
    pub text: String,
    pub author: Option<String>,
    pub channel: Option<String>,
}

pub fn parse_search(input: &str) -> SearchInput {
    let mut search = SearchInput::default();
    let mut words = Vec::new();
    for word in input.split_whitespace() {
        let lower = word.to_lowercase();
        if let Some(name) = lower.strip_prefix("de:").filter(|name| !name.is_empty()) {
            search.author = Some(name.trim_start_matches('@').to_owned());
        } else if let Some(name) = lower.strip_prefix("dans:").filter(|name| !name.is_empty()) {
            search.channel = Some(name.trim_start_matches('#').to_owned());
        } else {
            words.push(word);
        }
    }
    search.text = words.join(" ");
    search
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn search_filters() {
        assert_eq!(
            parse_search("  été  de:@Alice dans:#Général vacances "),
            SearchInput {
                text: "été vacances".into(), author: Some("alice".into()), channel: Some("général".into())
            }
        );
        assert_eq!(parse_search("de: rien"), SearchInput { text: "de: rien".into(), ..SearchInput::default() });
    }
}
