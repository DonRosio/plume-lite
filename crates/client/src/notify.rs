//! System notifications for new messages: the desktop's notification service over D-Bus on
//! Linux, toasts on Windows. Shown from a background thread: a slow notification service must
//! never freeze the window.

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::markup::{self, Block, Inline};
use plume_lite_protocol::model::{Message, NotificationLevel};

/// Characters of the message shown in a notification.
const BODY_MAX: usize = 200;

/// Whether a new message deserves a notification, given the setting of its channel.
pub fn should_notify(
    level: NotificationLevel,
    for_me: bool,
    mine: bool,
    do_not_disturb: bool,
    on_screen: bool,
) -> bool {
    let wanted = match level {
        NotificationLevel::All => true,
        NotificationLevel::Mentions => for_me,
        NotificationLevel::Nothing => false,
    };
    wanted && !mine && !do_not_disturb && !on_screen
}

/// The message as plain text: mentions by name, spoilers hidden, on one line.
pub fn body(message: &Message, name_of: impl Fn(Snowflake) -> Option<String>) -> String {
    fn collect(blocks: &[Block], name_of: &dyn Fn(Snowflake) -> Option<String>, out: &mut String) {
        for block in blocks {
            if !out.is_empty() {
                out.push(' ');
            }
            let runs = match block {
                Block::Paragraph(runs) | Block::Heading(_, runs) | Block::Item(runs) => runs,
                Block::Quote(inner) => {
                    collect(inner, name_of, out);
                    continue;
                }
                Block::Code { code, .. } => {
                    out.push_str(code);
                    continue;
                }
            };
            for run in runs {
                if run.style.spoiler.is_some() {
                    out.push_str("▒▒▒");
                    continue;
                }
                match &run.inline {
                    Inline::Text(text) | Inline::Link(text) => out.push_str(text),
                    Inline::User(id) => {
                        out.push('@');
                        out.push_str(&name_of(*id).unwrap_or_else(|| "utilisateur".to_owned()));
                    }
                    Inline::Everyone => out.push_str("@everyone"),
                }
            }
        }
    }
    let mut text = String::new();
    collect(&markup::parse(&message.content), &name_of, &mut text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(BODY_MAX) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text,
    }
}

/// Shows a notification, without waiting.
pub fn show(title: String, body: String) {
    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification.summary(&title).body(&body).appname("Plume Lite");
        #[cfg(windows)]
        if windows::registered() {
            notification.app_id(windows::APP_ID);
        }
        if let Err(err) = notification.show() {
            eprintln!("cannot show a notification: {err}");
        }
    });
}

/// Windows names the sender of a toast after an application id. A program that is not installed
/// from the Store declares its own under the current user's key, once, with the name to show.
#[cfg(windows)]
mod windows {
    use std::sync::OnceLock;

    pub const APP_ID: &str = "DonRGames.PlumeLite";

    /// Without it, toasts would show under PowerShell's name: better than none.
    pub fn registered() -> bool {
        static REGISTERED: OnceLock<bool> = OnceLock::new();
        *REGISTERED.get_or_init(|| {
            let key = windows_registry::CURRENT_USER.create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
            match key.and_then(|key| key.set_string("DisplayName", "Plume Lite")) {
                Ok(()) => true,
                Err(err) => {
                    eprintln!("cannot register the notification name: {err}");
                    false
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use plume_lite_protocol::model::User;

    use super::*;

    #[test]
    fn who_gets_notified() {
        use NotificationLevel::*;
        assert!(should_notify(All, false, false, false, false));
        assert!(!should_notify(Mentions, false, false, false, false));
        assert!(should_notify(Mentions, true, false, false, false));
        assert!(!should_notify(Nothing, true, false, false, false));
        assert!(!should_notify(All, true, true, false, false), "our own messages");
        assert!(!should_notify(All, true, false, true, false), "do not disturb");
        assert!(!should_notify(All, true, false, false, true), "already on screen");
    }

    #[test]
    fn bodies_are_plain_text() {
        let message = |content: &str| Message {
            id: Snowflake(1),
            channel_id: Snowflake(2),
            guild_id: Snowflake(3),
            author: User { id: Snowflake(4), username: "alice".into() },
            content: content.into(),
            kind: Default::default(),
            created_at: Utc::now(),
            edited_at: None,
            reply_to: None,
            mention_everyone: false,
            reactions: vec![],
            pinned: false,
            nonce: None,
        };
        let name_of = |id: Snowflake| (id == Snowflake(5)).then(|| "bob".to_owned());
        assert_eq!(body(&message("**Salut** <@5>,\n> regarde ||ça||"), name_of), "Salut @bob, regarde ▒▒▒");
        assert_eq!(body(&message(&"é".repeat(300)), name_of).chars().count(), BODY_MAX + 1);
    }
}
