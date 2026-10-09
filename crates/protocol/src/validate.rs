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
pub const MESSAGE_MAX: usize = 2000;
pub const INVITE_MAX_USES: i32 = 10_000;
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

/// Returns the trimmed content.
pub fn message_content(value: &str) -> Result<String, Invalid> {
    let content = value.trim();
    if content.is_empty() {
        return Err("Le message est vide.");
    }
    if content.chars().count() > MESSAGE_MAX {
        return Err("Le message dépasse 2000 caractères.");
    }
    if content.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("Le message contient des caractères invalides.");
    }
    Ok(content.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn guild_names() {
        assert_eq!(guild_name("  Rust FR  ").unwrap(), "Rust FR");
        assert!(guild_name("x").is_err());
        assert!(guild_name("bad\u{7}name").is_err());
    }
}
