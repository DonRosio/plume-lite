//! Input rules. The server is the authority; clients run the same checks for instant feedback.
//!
//! Error strings are user-facing (French UI).

pub const USERNAME_MIN: usize = 2;
pub const USERNAME_MAX: usize = 32;
pub const PASSWORD_MIN: usize = 8;
pub const PASSWORD_MAX: usize = 128;
pub const GUILD_NAME_MIN: usize = 2;
pub const GUILD_NAME_MAX: usize = 100;
pub const CHANNEL_NAME_MAX: usize = 100;
pub const CATEGORY_NAME_MAX: usize = 100;
pub const BAN_REASON_MAX: usize = 512;
pub const ROLE_NAME_MAX: usize = 100;
pub const WELCOME_MESSAGE_MAX: usize = 500;
/// Highest colour of a role (`0xRRGGBB`).
pub const ROLE_COLOR_MAX: u32 = 0xFF_FFFF;
pub const MESSAGE_MAX: usize = 4000;
pub const INVITE_MAX_USES: i32 = 10_000;
/// Different emojis under one message.
pub const REACTIONS_PER_MESSAGE_MAX: usize = 20;
/// Pinned messages in one channel.
pub const PINS_PER_CHANNEL_MAX: i64 = 50;
/// Bytes of one reaction (an emoji with skin tone, flag or family sequence fits).
pub const REACTION_MAX_BYTES: usize = 32;
pub const INVITE_MAX_AGE_SECS: i64 = 30 * 24 * 3600;

pub type Invalid = &'static str;

pub fn username(value: &str) -> Result<(), Invalid> {
    let len = value.chars().count();
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&len) {
        return Err("Le nom d'utilisateur doit faire entre 2 et 32 caractères.");
    }
    if !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return Err("Le nom d'utilisateur ne peut contenir que des lettres, chiffres, « _ » et « . ».");
    }
    Ok(())
}

pub fn password(value: &str) -> Result<(), Invalid> {
    let len = value.chars().count();
    if len < PASSWORD_MIN {
        return Err("Le mot de passe doit faire au moins 8 caractères.");
    }
    if len > PASSWORD_MAX {
        return Err("Le mot de passe est trop long (128 caractères max).");
    }
    if value.contains('\0') {
        return Err("Le mot de passe contient un caractère invalide.");
    }
    Ok(())
}

/// Returns the trimmed name.
pub fn guild_name(value: &str) -> Result<String, Invalid> {
    let name = value.trim();
    let len = name.chars().count();
    if !(GUILD_NAME_MIN..=GUILD_NAME_MAX).contains(&len) {
        return Err("Le nom du serveur doit faire entre 2 et 100 caractères.");
    }
    if name.chars().any(char::is_control) {
        return Err("Le nom du serveur contient des caractères invalides.");
    }
    Ok(name.to_owned())
}

/// Normalises a channel name the way Discord does: `" Général Chat "` → `"général-chat"`.
pub fn channel_name(value: &str) -> Result<String, Invalid> {
    let mut name = String::with_capacity(value.len());
    for c in value.trim().chars().flat_map(char::to_lowercase) {
        let c = if c.is_whitespace() || c == '-' { '-' } else { c };
        if c == '-' && (name.is_empty() || name.ends_with('-')) {
            continue;
        }
        if c == '-' || c == '_' || c.is_alphanumeric() {
            name.push(c);
        }
    }
    let name = name.trim_end_matches('-').to_owned();
    match name.chars().count() {
        0 => Err("Le nom du salon doit contenir au moins une lettre ou un chiffre."),
        n if n > CHANNEL_NAME_MAX => Err("Le nom du salon fait plus de 100 caractères."),
        _ => Ok(name),
    }
}

/// Returns the trimmed name (shown in capitals, kept as typed).
pub fn category_name(value: &str) -> Result<String, Invalid> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > CATEGORY_NAME_MAX {
        return Err("Le nom de la catégorie doit faire entre 1 et 100 caractères.");
    }
    if name.chars().any(char::is_control) {
        return Err("Le nom de la catégorie contient des caractères invalides.");
    }
    Ok(name.to_owned())
}

/// Returns the trimmed name. `@everyone` and `@here` are taken.
pub fn role_name(value: &str) -> Result<String, Invalid> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > ROLE_NAME_MAX {
        return Err("Le nom du rôle doit faire entre 1 et 100 caractères.");
    }
    if name.chars().any(char::is_control) {
        return Err("Le nom du rôle contient des caractères invalides.");
    }
    if name.eq_ignore_ascii_case("@everyone") || name.eq_ignore_ascii_case("@here") {
        return Err("Ce nom de rôle est réservé.");
    }
    Ok(name.to_owned())
}

pub fn role_color(value: u32) -> Result<u32, Invalid> {
    if value > ROLE_COLOR_MAX {
        return Err("Couleur invalide.");
    }
    Ok(value)
}

/// Returns the trimmed welcome message, `None` when empty (the default one is used).
pub fn welcome_message(value: &str) -> Result<Option<String>, Invalid> {
    let message = value.trim();
    if message.chars().count() > WELCOME_MESSAGE_MAX {
        return Err("Le message de bienvenue dépasse 500 caractères.");
    }
    if message.chars().any(|c| c.is_control() && c != '\n') {
        return Err("Le message de bienvenue contient des caractères invalides.");
    }
    Ok((!message.is_empty()).then(|| message.to_owned()))
}

/// Returns the trimmed reason, `None` when there is none.
pub fn ban_reason(value: &str) -> Result<Option<String>, Invalid> {
    let reason = value.trim();
    if reason.chars().count() > BAN_REASON_MAX {
        return Err("La raison dépasse 512 caractères.");
    }
    if reason.chars().any(char::is_control) {
        return Err("La raison contient des caractères invalides.");
    }
    Ok((!reason.is_empty()).then(|| reason.to_owned()))
}

/// Returns the trimmed content.
pub fn message_content(value: &str) -> Result<String, Invalid> {
    let content = value.trim();
    if content.is_empty() {
        return Err("Le message est vide.");
    }
    if content.chars().count() > MESSAGE_MAX {
        return Err("Le message dépasse 4000 caractères.");
    }
    if content.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("Le message contient des caractères invalides.");
    }
    Ok(content.to_owned())
}

/// A reaction must be one Unicode emoji (custom emojis come later): emoji characters, plus
/// what composes them (joiner, variation selectors, skin tones, keycaps, flag tags).
pub fn reaction(value: &str) -> Result<(), Invalid> {
    let pictographic = |c: char| {
        matches!(c as u32,
            0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x21AA | 0x231A..=0x23FF
            | 0x24C2 | 0x25AA..=0x25FE | 0x2600..=0x27BF | 0x2934 | 0x2935 | 0x2B05..=0x2B55
            | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF)
    };
    let joining = |c: char| matches!(c as u32, 0x200D | 0xFE0E | 0xFE0F | 0x20E3 | 0xE0020..=0xE007F);
    let keycap = |c: char| c.is_ascii_digit() || c == '#' || c == '*';
    let valid = !value.is_empty()
        && value.len() <= REACTION_MAX_BYTES
        && (value.chars().any(pictographic) || value.contains('\u{20E3}'))
        && value.chars().all(|c| pictographic(c) || joining(c) || keycap(c));
    if valid { Ok(()) } else { Err("Cette réaction n'est pas un emoji.") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_are_emojis() {
        for emoji in ["👍", "❤️", "👍🏽", "🇫🇷", "👨‍👩‍👧", "1️⃣", "🏴‍☠️", "☕"]
        {
            assert!(reaction(emoji).is_ok(), "{emoji}");
        }
        for text in ["", "a", "lol", "👍 ", "👍a", "1", "\u{200D}", &"👍".repeat(9), "<@1>"] {
            assert!(reaction(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn usernames() {
        assert!(username("vincent_05").is_ok());
        assert!(username("a").is_err());
        assert!(username("with space").is_err());
        assert!(username("émile").is_err());
        assert!(username(&"x".repeat(33)).is_err());
    }

    #[test]
    fn channel_names_are_normalised() {
        assert_eq!(channel_name("  Général Chat ").unwrap(), "général-chat");
        assert_eq!(channel_name("a -- b").unwrap(), "a-b");
        assert_eq!(channel_name("rust!!!").unwrap(), "rust");
        assert!(channel_name("!!!").is_err());
        assert!(channel_name("   ").is_err());
    }

    #[test]
    fn message_content_rules() {
        assert_eq!(message_content("  salut \n").unwrap(), "salut");
        assert!(message_content("ligne 1\nligne 2").is_ok());
        assert!(message_content("   ").is_err());
        assert!(message_content("nul\0byte").is_err());
        assert!(message_content(&"é".repeat(MESSAGE_MAX)).is_ok());
        assert!(message_content(&"é".repeat(MESSAGE_MAX + 1)).is_err());
    }

    #[test]
    fn category_names_and_ban_reasons() {
        assert_eq!(category_name("  Jeux vidéo ").unwrap(), "Jeux vidéo");
        assert!(category_name("  ").is_err());
        assert!(category_name("a\nb").is_err());
        assert!(category_name(&"x".repeat(CATEGORY_NAME_MAX + 1)).is_err());
        assert_eq!(ban_reason("  ").unwrap(), None);
        assert_eq!(ban_reason(" spam ").unwrap().as_deref(), Some("spam"));
        assert!(ban_reason(&"x".repeat(BAN_REASON_MAX + 1)).is_err());
        assert!(ban_reason("a\0b").is_err());
    }

    #[test]
    fn role_names_and_colors() {
        assert_eq!(role_name("  Modération ").unwrap(), "Modération");
        assert!(role_name("").is_err());
        assert!(role_name("@Everyone").is_err());
        assert!(role_name("a\tb").is_err());
        assert!(role_color(0xFF_FFFF).is_ok());
        assert!(role_color(0x100_0000).is_err());
    }

    #[test]
    fn guild_names() {
        assert_eq!(guild_name("  Rust FR  ").unwrap(), "Rust FR");
        assert!(guild_name("x").is_err());
        assert!(guild_name("bad\u{7}name").is_err());
    }
}
