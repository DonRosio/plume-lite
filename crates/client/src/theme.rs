//! Visual identity: a light blue and pale pink theme (default) or a night blue one, and the
//! widget styles built on them.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::theme::Palette;
use iced::widget::{button, container, text_editor, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector, border, font};
use plume_lite_protocol::Snowflake;

const fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Every colour of one theme. Views read the active set through [`c()`].
pub struct Colors {
    pub rail: Color,
    pub sidebar: Color,
    pub chat: Color,
    pub panel: Color,
    pub input: Color,
    /// Background of standalone text fields (login, modals).
    pub field: Color,
    pub hover: Color,
    pub selected: Color,
    pub message_hover: Color,
    pub text: Color,
    pub muted: Color,
    pub bright: Color,
    pub placeholder: Color,
    pub accent: Color,
    pub accent_hover: Color,
    /// Mentions and unread badges.
    pub pink: Color,
    /// Background of a message that mentions us (with a `pink` bar).
    pub mention: Color,
    /// Background of `@name` in messages.
    pub mention_pill: Color,
    /// Background of inline code and code blocks.
    pub code: Color,
    /// A spoiler, before it is clicked.
    pub spoiler: Color,
    /// The bar on the left of quotes.
    pub quote_bar: Color,
    pub link: Color,
    pub green: Color,
    pub red: Color,
    pub red_hover: Color,
    pub yellow: Color,
    /// Text drawn on `accent`, `red`, `green` and avatar fills.
    pub on_fill: Color,
    /// Opacity of the shadow under cards and of the modal backdrop.
    pub shadow: f32,
    pub backdrop: f32,
    /// Name and avatar colours, picked per user.
    pub users: [Color; 8],
}

/// Default theme: light blue with pale pink accents.
pub static LIGHT: Colors = Colors {
    rail: hex(0xb4d0f0),
    sidebar: hex(0xcfe2f7),
    chat: hex(0xe3eefa),
    panel: hex(0xc3daf3),
    input: hex(0xf3f8fe),
    field: hex(0xf3f8fe),
    hover: hex(0xbfd7f2),
    selected: hex(0xaecbee),
    message_hover: hex(0xd7e6f8),
    text: hex(0x1d2939),
    muted: hex(0x42526a),
    bright: hex(0x0e1a2c),
    placeholder: hex(0x5f6f88),
    accent: hex(0x2b67ad),
    accent_hover: hex(0x235792),
    pink: hex(0xb83e68),
    mention: hex(0xf3e0ec),
    mention_pill: hex(0xc6daf3),
    code: hex(0xd2e1f4),
    spoiler: hex(0x5f6f88),
    quote_bar: hex(0x9fb9db),
    link: hex(0x1f5ca3),
    green: hex(0x18703d),
    red: hex(0xb02e2e),
    red_hover: hex(0x922424),
    yellow: hex(0xe0a82e),
    on_fill: hex(0xffffff),
    shadow: 0.18,
    backdrop: 0.45,
    users: [
        hex(0x2b67ad),
        hex(0x18703d),
        hex(0xa15c00),
        hex(0xb02e2e),
        hex(0xa8326a),
        hex(0x0f766e),
        hex(0x6d3fb3),
        hex(0xa34a0e),
    ],
};

/// Night blue with the same blue and pink accents, lightened.
pub static DARK: Colors = Colors {
    rail: hex(0x10141c),
    sidebar: hex(0x1e2431),
    chat: hex(0x171c27),
    panel: hex(0x141922),
    input: hex(0x232a39),
    field: hex(0x10141c),
    hover: hex(0x283041),
    selected: hex(0x2e3850),
    message_hover: hex(0x1c2230),
    text: hex(0xe6ecf5),
    muted: hex(0x9aa7ba),
    bright: hex(0xf5f8fc),
    placeholder: hex(0x7d8aa0),
    accent: hex(0x8db8ec),
    accent_hover: hex(0xa6c8f1),
    pink: hex(0xf2a9c2),
    mention: hex(0x2a2232),
    mention_pill: hex(0x2c3c5a),
    code: hex(0x10141c),
    spoiler: hex(0x3a4356),
    quote_bar: hex(0x3d4760),
    link: hex(0x8db8ec),
    green: hex(0x4cc38a),
    red: hex(0xf07a7a),
    red_hover: hex(0xf39a9a),
    yellow: hex(0xe8b84a),
    on_fill: hex(0x10141c),
    shadow: 0.5,
    backdrop: 0.75,
    users: [
        hex(0x8db8ec),
        hex(0x4cc38a),
        hex(0xf0b85a),
        hex(0xf07a7a),
        hex(0xf2a9c2),
        hex(0x4fd1c5),
        hex(0xb9a0f0),
        hex(0xf59e62),
    ],
};

/// Dark text for the warning banner, whatever the theme (it sits on `yellow`).
const ON_YELLOW: Color = hex(0x1d2939);

/// The bundled font (see `main.rs`), also the application's default one.
pub const REGULAR: Font = Font::with_name("Noto Sans");
pub const BOLD: Font = Font { weight: font::Weight::Bold, ..REGULAR };
pub const SEMIBOLD: Font = Font { weight: font::Weight::Semibold, ..REGULAR };
pub const ITALIC: Font = Font { style: font::Style::Italic, ..REGULAR };

/// The font of message text with the given emphasis.
pub fn message_font(bold: bool, italic: bool, code: bool) -> Font {
    let base = if code { Font::MONOSPACE } else { REGULAR };
    Font {
        weight: if bold { font::Weight::Bold } else { base.weight },
        style: if italic { font::Style::Italic } else { font::Style::Normal },
        ..base
    }
}

/// Mirrors the user's choice. Only `App::update` writes it (through [`set_dark`]), so views,
/// which run after each update, always see the same theme as `App::theme`.
static DARK_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_dark(dark: bool) {
    DARK_MODE.store(dark, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK_MODE.load(Ordering::Relaxed)
}

/// The colours of the active theme.
pub fn c() -> &'static Colors {
    if is_dark() { &DARK } else { &LIGHT }
}

static LIGHT_THEME: LazyLock<Theme> = LazyLock::new(|| custom("Plume Lite", &LIGHT));
static DARK_THEME: LazyLock<Theme> = LazyLock::new(|| custom("Plume Lite sombre", &DARK));

fn custom(name: &'static str, colors: &Colors) -> Theme {
    Theme::custom(
        name,
        Palette {
            background: colors.chat,
            text: colors.text,
            primary: colors.accent,
            success: colors.green,
            warning: colors.yellow,
            danger: colors.red,
        },
    )
}

pub fn theme() -> Theme {
    if is_dark() { DARK_THEME.clone() } else { LIGHT_THEME.clone() }
}

/// Stable colour per user (avatars, names).
pub fn user_color(id: Snowflake) -> Color {
    let users = &c().users;
    let hash = (id.0 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32;
    users[(hash % users.len() as u64) as usize]
}

// --- Containers ---------------------------------------------------------------------------------

pub fn fill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style { background: Some(color.into()), ..container::Style::default() }
}

pub fn rounded_fill(color: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(color.into()),
        border: border::rounded(radius),
        ..container::Style::default()
    }
}

pub fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(c().chat.into()),
        text_color: Some(c().text),
        border: border::rounded(8),
        shadow: Shadow {
            color: Color { a: c().shadow, ..Color::BLACK },
            offset: Vector::new(0.0, 8.0),
            blur_radius: 24.0,
        },
        ..container::Style::default()
    }
}

pub fn backdrop(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color { a: c().backdrop, ..Color::BLACK }.into()),
        ..container::Style::default()
    }
}

pub fn toast(_: &Theme) -> container::Style {
    container::Style {
        background: Some(c().red.into()),
        text_color: Some(c().on_fill),
        border: border::rounded(6),
        ..container::Style::default()
    }
}

pub fn banner(_: &Theme) -> container::Style {
    container::Style { background: Some(c().yellow.into()), text_color: Some(ON_YELLOW), ..container::Style::default() }
}

// --- Buttons ------------------------------------------------------------------------------------

fn flat(background: Option<Color>, text: Color, radius: f32) -> button::Style {
    button::Style {
        background: background.map(Background::Color),
        text_color: text,
        border: border::rounded(radius),
        ..button::Style::default()
    }
}

/// Guild icon in the left rail: a circle that morphs into a rounded square when active.
pub fn guild_icon(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let active = selected || matches!(status, button::Status::Hovered | button::Status::Pressed);
        let c = c();
        let (background, text) = if active { (c.accent, c.on_fill) } else { (c.input, c.bright) };
        flat(Some(background), text, if active { 16.0 } else { 24.0 })
    }
}

/// "+" / "join" actions in the rail.
pub fn rail_action(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => flat(Some(c().green), c().on_fill, 16.0),
        _ => flat(Some(c().input), c().green, 24.0),
    }
}

pub fn list_item(selected: bool, highlighted: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = if selected {
            Some(c().selected)
        } else if hovered {
            Some(c().hover)
        } else {
            None
        };
        let text = if selected || highlighted || hovered { c().bright } else { c().muted };
        flat(background, text, 4.0)
    }
}

pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => flat(Some(c().accent_hover), c().on_fill, 4.0),
        button::Status::Disabled => flat(Some(Color { a: 0.5, ..c().accent }), Color { a: 0.6, ..c().on_fill }, 4.0),
        button::Status::Active => flat(Some(c().accent), c().on_fill, 4.0),
    }
}

pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => flat(Some(c().red_hover), c().on_fill, 4.0),
        button::Status::Disabled => flat(Some(Color { a: 0.5, ..c().red }), Color { a: 0.6, ..c().on_fill }, 4.0),
        button::Status::Active => flat(Some(c().red), c().on_fill, 4.0),
    }
}

/// Text-only button (secondary actions, links).
pub fn ghost(_: &Theme, status: button::Status) -> button::Style {
    match status {
        button::Status::Hovered | button::Status::Pressed => flat(Some(c().hover), c().bright, 4.0),
        _ => flat(None, c().muted, 4.0),
    }
}

pub fn link(_: &Theme, status: button::Status) -> button::Style {
    let color = if matches!(status, button::Status::Hovered) { c().bright } else { c().link };
    flat(None, color, 0.0)
}

/// Small icon-like action shown on a hovered message.
pub fn message_action(danger: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let text = match (danger, hovered) {
            (true, _) => c().red,
            (false, true) => c().bright,
            (false, false) => c().muted,
        };
        flat(Some(if hovered { c().selected } else { c().sidebar }), text, 4.0)
    }
}

/// A reaction under a message: outlined in the accent colour when it is ours.
pub fn reaction(mine: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let c = c();
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if mine {
                c.mention_pill
            } else if hovered {
                c.hover
            } else {
                c.sidebar
            })),
            text_color: c.text,
            border: Border { color: if mine { c.accent } else { Color::TRANSPARENT }, width: 1.0, radius: 10.0.into() },
            ..button::Style::default()
        }
    }
}

// --- Inputs -------------------------------------------------------------------------------------

pub fn field(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: c().field.into(),
        border: Border { color: if focused { c().accent } else { c().field }, width: 1.0, radius: 4.0.into() },
        icon: c().muted,
        placeholder: c().placeholder,
        value: c().text,
        selection: Color { a: 0.4, ..c().accent },
    }
}

/// Borderless multi-line input living inside a styled container (composer, inline edit).
pub fn bare_editor(_: &Theme, _: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Color::TRANSPARENT.into(),
        border: Border::default(),
        placeholder: c().placeholder,
        value: c().text,
        selection: Color { a: 0.4, ..c().accent },
    }
}
