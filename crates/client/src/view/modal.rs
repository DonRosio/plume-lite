use iced::widget::text::Span;
use iced::widget::{
    Column, Row, button, center, checkbox, column, container, mouse_area, opaque, pick_list, rich_text, row,
    scrollable, space, span, stack, text, text_input,
};
use iced::{Color, Element, Fill};

use plume_lite_protocol::Snowflake;
use plume_lite_protocol::markup::Block;
use plume_lite_protocol::model::{AuditAction, AuditEntry, Ban, Channel, ChannelKind, Message, Role};
use plume_lite_protocol::permissions::{Overwrite, Permissions};

use super::markup::{self, Context};
use super::message_time;
use crate::app::{Chat, MODAL_INPUT, Modal, ModalKind, ModalRequest, Msg};
use crate::theme;

/// Draws `modal` above `base`; clicking the backdrop closes it.
pub fn overlay<'a>(base: Element<'a, Msg>, chat: &'a Chat, modal: &'a Modal) -> Element<'a, Msg> {
    let card = container(content(chat, modal)).width(460).padding(24).style(theme::card);
    stack![base, opaque(mouse_area(center(opaque(card)).style(theme::backdrop)).on_press(Msg::CloseModal)),].into()
}

fn content<'a>(chat: &'a Chat, modal: &'a Modal) -> Element<'a, Msg> {
    let guild_name = |id| chat.store.guild(id).map_or("ce serveur", |g| g.name.as_str());
    let channel_name = |id| chat.store.channel(id).map_or("ce salon", |c| c.name.as_str());
    let category_name = |id| chat.store.category(id).map_or("cette catégorie", |c| c.name.as_str());
    let user_name = |guild_id, user_id| chat.store.user_name(guild_id, user_id).unwrap_or_else(|| "ce membre".into());

    // Widgets shown under the input (choices of the channel modals).
    let mut extra: Option<Element<'a, Msg>> = None;
    let mut secure = false;
    let (title, description, input, confirm, danger): (String, String, Option<&str>, &str, bool) = match &modal.kind {
        ModalKind::CreateGuild => (
            "Créer un serveur".into(),
            "Votre serveur est l'endroit où vous retrouvez vos amis. Vous pourrez inviter du monde ensuite.".into(),
            Some("Nom du serveur"),
            "Créer",
            false,
        ),
        ModalKind::JoinGuild => (
            "Rejoindre un serveur".into(),
            "Collez le code ou le lien d'invitation que l'on vous a envoyé.".into(),
            Some("AbCd1234 ou https://…/invite/AbCd1234"),
            "Rejoindre",
            false,
        ),
        ModalKind::CreateChannel { guild_id, kind, category, private } => {
            let kinds = row![
                choice("# Textuel", *kind == ChannelKind::Text, Msg::ModalChannelKind(ChannelKind::Text)),
                choice("🔊 Vocal", *kind == ChannelKind::Voice, Msg::ModalChannelKind(ChannelKind::Voice)),
            ]
            .spacing(8);
            let private = column![
                checkbox(*private).label("Salon privé").on_toggle(Msg::ModalPrivate),
                text(
                    "Caché à @everyone : seuls vous et votre rôle le plus haut le verront. Vous pourrez ensuite \
                     l'ouvrir à d'autres rôles (✏)."
                )
                .size(12)
                .color(theme::c().muted),
            ]
            .spacing(4);
            extra =
                Some(column![kinds].push(category_picker(chat, *guild_id, *category)).push(private).spacing(12).into());
            (
                "Créer un salon".into(),
                "Les noms sont normalisés : « Mon Salon » devient « mon-salon ».".into(),
                Some("nouveau-salon"),
                "Créer le salon",
                false,
            )
        }
        ModalKind::EditChannel { channel_id, category, overwrites } => {
            if let Some(channel) = chat.store.channel(*channel_id) {
                extra = Some(
                    column![]
                        .push(category_picker(chat, channel.guild_id, *category))
                        .push(rights_editor(chat, channel, overwrites))
                        .spacing(16)
                        .into(),
                );
            }
            (
                format!("Modifier #{}", channel_name(*channel_id)),
                "Les noms sont normalisés : « Mon Salon » devient « mon-salon ».".into(),
                Some("nom-du-salon"),
                "Enregistrer",
                false,
            )
        }
        ModalKind::GuildSettings { guild_id, welcome_channel, welcome_message } => {
            extra = Some(welcome_settings(chat, *guild_id, *welcome_channel, welcome_message));
            (
                format!("Paramètres de « {} »", guild_name(*guild_id)),
                "Le nom du serveur, et l'accueil des nouveaux membres.".into(),
                Some("Nom du serveur"),
                "Enregistrer",
                false,
            )
        }
        ModalKind::AuditLog { guild_id, entries, next, loading } => {
            return audit_content(chat, *guild_id, entries, *next, *loading, modal);
        }
        ModalKind::CreateCategory(_) => (
            "Créer une catégorie".into(),
            "Une catégorie regroupe des salons sous un même titre.".into(),
            Some("Nom de la catégorie"),
            "Créer",
            false,
        ),
        ModalKind::RenameCategory(id) => (
            format!("Renommer « {} »", category_name(*id)),
            "Tous les membres verront le nouveau nom.".into(),
            Some("Nom de la catégorie"),
            "Renommer",
            false,
        ),
        ModalKind::DeleteCategory(id) => (
            format!("Supprimer « {} »", category_name(*id)),
            "Ses salons ne sont pas supprimés : ils remontent au-dessus des catégories.".into(),
            None,
            "Supprimer la catégorie",
            true,
        ),
        ModalKind::Member { guild_id, user_id } => {
            return member_content(chat, *guild_id, *user_id);
        }
        ModalKind::Kick { guild_id, user_id } => (
            format!("Exclure {}", user_name(*guild_id, *user_id)),
            "Ce membre quitte le serveur. Il pourra revenir avec une nouvelle invitation.".into(),
            None,
            "Exclure",
            true,
        ),
        ModalKind::Ban { guild_id, user_id } => (
            format!("Bannir {}", user_name(*guild_id, *user_id)),
            "Ce membre quitte le serveur et ne pourra plus le rejoindre, même avec une invitation. Les invitations \
             qu'il a créées sont supprimées. Vous pourrez lever le bannissement depuis « Membres bannis »."
                .into(),
            Some("Raison (facultative)"),
            "Bannir",
            true,
        ),
        ModalKind::Transfer { guild_id, user_id } => {
            secure = true;
            (
                format!("Transférer « {} »", guild_name(*guild_id)),
                format!(
                    "{} deviendra propriétaire du serveur : vous ne pourrez plus le gérer ni le supprimer, et lui seul \
                     pourra vous rendre la propriété. Confirmez avec votre mot de passe.",
                    user_name(*guild_id, *user_id)
                ),
                Some("Votre mot de passe"),
                "Transférer la propriété",
                true,
            )
        }
        ModalKind::Roles(guild_id) => {
            return roles_content(chat, *guild_id);
        }
        ModalKind::CreateRole(_) => (
            "Créer un rôle".into(),
            "Vous choisirez ensuite sa couleur et ses permissions.".into(),
            Some("Nom du rôle"),
            "Créer",
            false,
        ),
        ModalKind::EditRole { role_id, color, permissions } => {
            let Some(role) = chat.store.role(*role_id) else {
                return text("Ce rôle n'existe plus.").color(theme::c().muted).into();
            };
            extra = Some(role_editor(chat, role, *color, *permissions));
            if role.is_everyone() {
                (
                    "Permissions de @everyone".into(),
                    "Ce que peut faire chaque membre du serveur, quels que soient ses rôles.".into(),
                    None,
                    "Enregistrer",
                    false,
                )
            } else {
                (
                    format!("Modifier le rôle « {} »", role.name),
                    "Ses membres ont ces permissions en plus de celles de @everyone.".into(),
                    Some("Nom du rôle"),
                    "Enregistrer",
                    false,
                )
            }
        }
        ModalKind::DeleteRole(id) => (
            format!("Supprimer le rôle « {} »", chat.store.role(*id).map_or("ce rôle", |r| r.name.as_str())),
            "Ses membres le perdent, avec les permissions qu'il leur donnait.".into(),
            None,
            "Supprimer le rôle",
            true,
        ),
        ModalKind::Bans { guild_id, bans } => {
            return bans_content(*guild_id, bans.as_deref(), modal);
        }
        ModalKind::Invite { invite } => {
            return invite_content(invite.as_ref(), modal);
        }
        ModalKind::VoiceMember { guild_id, user_id } => {
            return voice_member_content(chat, *guild_id, *user_id);
        }
        ModalKind::Pins { channel_id, messages } => {
            return pins_content(chat, *channel_id, messages.as_deref(), modal);
        }
        ModalKind::LeaveGuild(id) => (
            format!("Quitter « {} »", guild_name(*id)),
            "Vous ne pourrez revenir qu'avec une nouvelle invitation.".into(),
            None,
            "Quitter le serveur",
            true,
        ),
        ModalKind::DeleteGuild(id) => (
            format!("Supprimer « {} »", guild_name(*id)),
            "Tous les salons et messages seront définitivement supprimés. Cette action est irréversible.".into(),
            None,
            "Supprimer le serveur",
            true,
        ),
        ModalKind::DeleteChannel(id) => (
            format!("Supprimer #{}", channel_name(*id)),
            "Tous les messages de ce salon seront définitivement supprimés.".into(),
            None,
            "Supprimer le salon",
            true,
        ),
    };

    let field = input.map(|placeholder| {
        text_input(placeholder, &modal.input)
            .id(MODAL_INPUT)
            .secure(secure)
            .on_input(Msg::ModalInput)
            .on_submit(Msg::SubmitModal)
            .padding(10)
            .style(theme::field)
    });
    let error = modal.error.as_deref().map(|e| text(e).size(14).color(theme::c().red));
    let confirm_button = button(text(if modal.busy { "Patientez…" } else { confirm }).font(theme::SEMIBOLD))
        .padding([10, 16])
        .style(if danger { theme::danger } else { theme::primary })
        .on_press_maybe((!modal.busy).then_some(Msg::SubmitModal));

    column![
        text(title).size(20).font(theme::BOLD).color(theme::c().bright),
        text(description).color(theme::c().muted),
        field,
        extra,
        error,
        row![
            space::horizontal(),
            button(text("Annuler")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal),
            confirm_button
        ]
        .spacing(8),
    ]
    .spacing(16)
    .into()
}

/// Who may see and write in a channel, role by role: following the guild (/), allowed (✓)
/// or denied (✗). Only roles ranked below ours (and @everyone), and only rights we have here.
fn rights_editor<'a>(chat: &'a Chat, channel: &'a Channel, overwrites: &'a [Overwrite]) -> Element<'a, Msg> {
    let c = theme::c();
    let me = chat.standing(channel.guild_id);
    let mine = chat.store.channel_permissions(channel.id, chat.me.id);
    let mut rows = Column::new().spacing(6);
    rows = rows.push(
        row![
            space::horizontal(),
            text("Voir").size(12).color(c.muted).width(96).center(),
            text("Écrire").size(12).color(c.muted).width(96).center()
        ]
        .spacing(8),
    );
    let mut roles = chat.store.roles_of(channel.guild_id);
    roles.rotate_right(1); // @everyone first
    for role in roles {
        let overwrite = overwrites.iter().find(|o| o.role_id == role.id);
        let editable = role.is_everyone() || me.above_role(role.position);
        let tri = |permission: Permissions| {
            let state = overwrite.and_then(|o| {
                if o.allow.contains(permission) {
                    Some(true)
                } else if o.deny.contains(permission) {
                    Some(false)
                } else {
                    None
                }
            });
            let allowed = editable && mine.contains(permission);
            let option = |label: &'static str, value: Option<bool>| {
                let selected = state == value;
                let msg = Msg::ModalOverwrite { role_id: role.id, permission, state: value };
                button(text(label).size(12).center().width(Fill))
                    .padding([3, 0])
                    .width(30)
                    .style(theme::list_item(selected, selected))
                    .on_press_maybe(allowed.then_some(msg))
            };
            row![option("/", None), option("✓", Some(true)), option("✗", Some(false))].spacing(3).width(96)
        };
        rows = rows.push(
            row![
                container(role_name(role)).width(Fill),
                tri(Permissions::VIEW_CHANNEL),
                tri(Permissions::SEND_MESSAGES)
            ]
            .spacing(8)
            .align_y(iced::Center),
        );
    }
    column![
        text("Droits dans ce salon").size(13).font(theme::SEMIBOLD).color(c.muted),
        text("/ : comme sur le serveur ; ✓ : autorisé ; ✗ : refusé. Un rôle qui autorise l'emporte sur un rôle qui refuse.")
            .size(12)
            .color(c.muted),
        container(scrollable(rows.padding(iced::Padding { right: 14.0, ..iced::Padding::ZERO }))).max_height(220),
    ]
    .spacing(6)
    .into()
}

/// One of a few exclusive choices.
fn choice<'a>(label: &'a str, selected: bool, msg: Msg) -> Element<'a, Msg> {
    button(text(label).size(14)).padding([6, 12]).style(theme::list_item(selected, selected)).on_press(msg).into()
}

/// One of a list of things (a category, a channel), or none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    id: Option<Snowflake>,
    name: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// "Catégorie : [Aucune ▾]", when the guild has categories.
fn category_picker<'a>(chat: &'a Chat, guild_id: Snowflake, selected: Option<Snowflake>) -> Option<Row<'a, Msg>> {
    let categories = chat.store.categories_of(guild_id);
    if categories.is_empty() {
        return None;
    }
    let none = Choice { id: None, name: "Aucune (au-dessus des catégories)".into() };
    let choices: Vec<Choice> = std::iter::once(none.clone())
        .chain(categories.iter().map(|c| Choice { id: Some(c.id), name: c.name.clone() }))
        .collect();
    let current = choices.iter().find(|c| c.id == selected).cloned().unwrap_or(none);
    Some(
        row![
            text("Catégorie").color(theme::c().muted),
            pick_list(choices, Some(current), |choice: Choice| Msg::ModalCategory(choice.id))
                .padding([6, 10])
                .width(Fill),
        ]
        .spacing(12)
        .align_y(iced::Center),
    )
}

/// Where newcomers are announced, and what is said to them.
fn welcome_settings<'a>(
    chat: &'a Chat,
    guild_id: Snowflake,
    selected: Option<Snowflake>,
    message: &'a str,
) -> Element<'a, Msg> {
    let c = theme::c();
    let none = Choice { id: None, name: "Aucun (pas d'annonce)".into() };
    let choices: Vec<Choice> = std::iter::once(none.clone())
        .chain(
            chat.store
                .channels_of(guild_id)
                .into_iter()
                .filter(|channel| channel.kind == ChannelKind::Text)
                .map(|channel| Choice { id: Some(channel.id), name: format!("#{}", channel.name) }),
        )
        .collect();
    let current = choices.iter().find(|choice| choice.id == selected).cloned().unwrap_or(none);
    column![
        text("Accueil").size(13).font(theme::SEMIBOLD).color(c.muted),
        row![
            text("Salon d'accueil").color(c.muted),
            pick_list(choices, Some(current), |choice: Choice| Msg::ModalWelcomeChannel(choice.id))
                .padding([6, 10])
                .width(Fill),
        ]
        .spacing(12)
        .align_y(iced::Center),
        text_input("Message de bienvenue (facultatif)", message)
            .on_input(Msg::ModalWelcomeMessage)
            .on_submit(Msg::SubmitModal)
            .padding(10)
            .style(theme::field),
        text("Chaque nouveau membre y est annoncé : « X a rejoint le serveur », suivi de ce message.")
            .size(12)
            .color(c.muted),
    ]
    .spacing(8)
    .into()
}

/// The audit log: one line per action, newest first, a page at a time.
fn audit_content<'a>(
    chat: &'a Chat,
    guild_id: Snowflake,
    entries: &'a [AuditEntry],
    next: Option<Snowflake>,
    loading: bool,
    modal: &'a Modal,
) -> Element<'a, Msg> {
    let c = theme::c();
    let mut list = Column::new().spacing(6);
    for entry in entries {
        let spans: Vec<Span<'a, (), iced::Font>> = audit_sentence(entry)
            .into_iter()
            .map(|(words, bold)| {
                let span = span(words).color(if bold { c.bright } else { c.text });
                if bold { span.font(theme::SEMIBOLD) } else { span }
            })
            .collect();
        let mut item =
            column![rich_text(spans).size(14), text(message_time(entry.id.created_at())).size(11).color(c.muted)]
                .spacing(2);
        if let Some(reason) = &entry.reason {
            item = item.push(text(format!("Raison : {reason}")).size(12).color(c.muted));
        }
        for change in &entry.changes {
            let value = |v: &Option<String>| v.clone().unwrap_or_else(|| "—".to_owned());
            let line = format!("{} : {} → {}", change.field, value(&change.before), value(&change.after));
            item = item.push(text(line).size(12).color(c.muted));
        }
        list = list.push(container(item).padding([6, 10]).width(Fill).style(theme::rounded_fill(c.chat, 6.0)));
    }
    let body: Element<'a, Msg> = match (&modal.error, entries.is_empty(), loading) {
        (Some(error), ..) => text(error).color(c.red).into(),
        (None, true, true) => text("Chargement…").color(c.muted).into(),
        (None, true, false) => text("Rien pour l'instant.").color(c.muted).into(),
        (None, false, _) => container(scrollable(list.padding(iced::Padding { right: 14.0, ..iced::Padding::ZERO })))
            .max_height(420)
            .into(),
    };
    let more = next.map(|_| {
        button(text(if loading { "Chargement…" } else { "Plus ancien" }).size(13))
            .padding([8, 12])
            .style(theme::ghost)
            .on_press_maybe((!loading).then_some(Msg::MoreAudit))
    });
    let name = chat.store.guild(guild_id).map_or("ce serveur", |g| g.name.as_str());
    column![
        text(format!("Journal de « {name} »")).size(20).font(theme::BOLD).color(c.bright),
        text("Ce que la modération a fait ces 90 derniers jours.").color(c.muted),
        body,
        row![space::horizontal()]
            .push(more)
            .push(button(text("Fermer")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal))
            .spacing(8),
    ]
    .spacing(16)
    .into()
}

/// An entry as a sentence: `(words, bold)` pieces, names in bold.
pub(crate) fn audit_sentence(entry: &AuditEntry) -> Vec<(String, bool)> {
    use AuditAction as A;
    let actor = entry.actor.as_ref().map_or_else(|| "Un compte supprimé".to_owned(), |u| u.username.clone());
    let user = entry.target_user.as_ref().map_or_else(|| "un compte supprimé".to_owned(), |u| u.username.clone());
    let name = entry.target_name.clone().unwrap_or_default();
    let plain = |words: &str| (words.to_owned(), false);
    let bold = |words: String| (words, true);
    let rest: Vec<(String, bool)> = match entry.action {
        A::GuildUpdate => vec![plain(" a modifié le serveur")],
        A::OwnershipTransfer => vec![plain(" a confié le serveur à "), bold(user)],
        A::ChannelCreate => vec![plain(" a créé le salon "), bold(name)],
        A::ChannelUpdate => vec![plain(" a modifié le salon "), bold(name)],
        A::ChannelDelete => vec![plain(" a supprimé le salon "), bold(name)],
        A::CategoryCreate => vec![plain(" a créé la catégorie "), bold(name)],
        A::CategoryUpdate => vec![plain(" a modifié la catégorie "), bold(name)],
        A::CategoryDelete => vec![plain(" a supprimé la catégorie "), bold(name)],
        A::RoleCreate => vec![plain(" a créé le rôle "), bold(name)],
        A::RoleUpdate => vec![plain(" a modifié le rôle "), bold(name)],
        A::RoleDelete => vec![plain(" a supprimé le rôle "), bold(name)],
        A::MemberRoleAdd => vec![plain(" a donné le rôle "), bold(name), plain(" à "), bold(user)],
        A::MemberRoleRemove => vec![plain(" a retiré le rôle "), bold(name), plain(" à "), bold(user)],
        A::MemberKick => vec![plain(" a exclu "), bold(user)],
        A::MemberBan => vec![plain(" a banni "), bold(user)],
        A::MemberUnban => vec![plain(" a levé le bannissement de "), bold(user)],
        A::MessageDelete => vec![plain(" a supprimé un message de "), bold(user), plain(" dans "), bold(name)],
        A::MessagePin => vec![plain(" a épinglé un message dans "), bold(name)],
        A::MessageUnpin => vec![plain(" a désépinglé un message dans "), bold(name)],
        A::VoiceMute => vec![plain(" a coupé le micro de "), bold(user)],
        A::VoiceUnmute => vec![plain(" a rendu la parole à "), bold(user)],
        A::VoiceDisconnect => vec![plain(" a sorti "), bold(user), plain(" du vocal")],
        A::VoiceMove => vec![plain(" a déplacé "), bold(user), plain(" vers "), bold(name)],
        A::InviteCreate => vec![plain(" a créé une invitation")],
        A::Unknown => vec![plain(" a fait une action que cette version ne connaît pas")],
    };
    std::iter::once((actor, true)).chain(rest).collect()
}

/// What we can do to a member: give or take roles, exclude, ban, hand the guild over.
fn member_content<'a>(chat: &'a Chat, guild_id: Snowflake, user_id: Snowflake) -> Element<'a, Msg> {
    let c = theme::c();
    let name = chat.store.user_name(guild_id, user_id).unwrap_or_else(|| "Ce membre".to_owned());
    let me = chat.standing(guild_id);
    let below = chat.outranks(guild_id, user_id);
    let action = |label: &'static str, danger: bool, request: ModalRequest| {
        button(text(label).font(theme::SEMIBOLD))
            .padding([10, 16])
            .style(if danger { theme::danger } else { theme::primary })
            .on_press(Msg::OpenModal(request))
    };

    // Roles: those ranked below ours, with permissions we have, on ourselves or someone lower.
    let roles = (me.can(Permissions::MANAGE_ROLES) && (below || user_id == chat.me.id)).then(|| {
        let theirs = chat.store.member_roles(guild_id, user_id, chat.me.id).unwrap_or_default();
        let mut list = Column::new().spacing(6);
        for role in chat.store.roles_of(guild_id).into_iter().filter(|r| !r.is_everyone()) {
            let has = theirs.contains(&role.id);
            let allowed = me.above_role(role.position) && (has || me.may_grant(Permissions::NONE, role.permissions));
            let on_toggle = allowed.then_some(move |on| Msg::SetMemberRole { guild_id, user_id, role_id: role.id, on });
            list = list
                .push(row![checkbox(has).on_toggle_maybe(on_toggle), role_name(role)].spacing(8).align_y(iced::Center));
        }
        column![
            text("Rôles").size(13).font(theme::SEMIBOLD).color(c.muted),
            container(scrollable(list)).max_height(220)
        ]
        .spacing(6)
    });
    let mut moderation = Row::new().spacing(8);
    if below && me.can(Permissions::KICK_MEMBERS) {
        moderation = moderation.push(action("Exclure", true, ModalRequest::Kick { guild_id, user_id }));
    }
    if below && me.can(Permissions::BAN_MEMBERS) {
        moderation = moderation.push(action("Bannir", true, ModalRequest::Ban { guild_id, user_id }));
    }
    let transfer = (me.owner && user_id != chat.me.id).then(|| {
        column![
            text("Lui confier le serveur").size(13).font(theme::SEMIBOLD).color(c.muted),
            action("Transférer la propriété", false, ModalRequest::Transfer { guild_id, user_id }),
        ]
        .spacing(6)
    });
    column![text(name).size(20).font(theme::BOLD).color(c.bright)]
        .push(roles)
        .push(moderation)
        .push(transfer)
        .push(row![
            space::horizontal(),
            button(text("Fermer")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal)
        ])
        .spacing(16)
        .into()
}

/// A role's name in its colour.
fn role_name(role: &Role) -> Element<'_, Msg> {
    let color = role.color.map_or(theme::c().text, theme::role_color);
    text(&role.name).font(theme::SEMIBOLD).color(color).into()
}

/// The roles of a guild, the strongest first: those ranked below ours can be moved, edited
/// and deleted; `@everyone` has only its permissions.
fn roles_content(chat: &Chat, guild_id: Snowflake) -> Element<'_, Msg> {
    let c = theme::c();
    let me = chat.standing(guild_id);
    let roles = chat.store.roles_of(guild_id);
    let highest = roles.iter().map(|r| r.position).max().unwrap_or(0);
    // Faded when it does nothing (a role we cannot manage, or already at that end).
    let small = |label: &'static str, msg: Option<Msg>| {
        let color = if msg.is_some() { c.text } else { Color { a: 0.3, ..c.muted } };
        button(text(label).size(13).color(color)).padding([4, 8]).style(theme::ghost).on_press_maybe(msg)
    };
    let mut list = Column::new().spacing(4);
    for role in roles {
        let editable = role.is_everyone() || me.above_role(role.position);
        let mut line = row![role_name(role), space::horizontal()].spacing(4).align_y(iced::Center);
        if !role.is_everyone() {
            let up = role.position < highest && me.above_role(role.position + 1);
            let down = role.position > 1;
            line = line
                .push(small("▲", (editable && up).then_some(Msg::MoveRole { role_id: role.id, up: true })))
                .push(small("▼", (editable && down).then_some(Msg::MoveRole { role_id: role.id, up: false })));
        }
        line = line.push(small("✏", editable.then_some(Msg::OpenModal(ModalRequest::EditRole(role.id)))));
        if !role.is_everyone() {
            line = line.push(small("🗑", editable.then_some(Msg::OpenModal(ModalRequest::DeleteRole(role.id)))));
        }
        list = list.push(container(line).padding([4, 10]).width(Fill).style(theme::rounded_fill(c.chat, 6.0)));
    }
    column![
        text("Rôles").size(20).font(theme::BOLD).color(c.bright),
        text(
            "Chaque membre a les permissions de @everyone et de chacun de ses rôles. Le rôle le plus haut \
             l'emporte : on ne gère que les rôles et les membres placés sous le sien."
        )
        .color(c.muted),
        container(scrollable(list)).max_height(360),
        row![
            space::horizontal(),
            button(text("Fermer")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal),
            button(text("Créer un rôle").font(theme::SEMIBOLD))
                .padding([10, 16])
                .style(theme::primary)
                .on_press(Msg::OpenModal(ModalRequest::CreateRole(guild_id))),
        ]
        .spacing(8),
    ]
    .spacing(16)
    .into()
}

/// Colour swatches (not for `@everyone`) and the permissions: only those we have can change.
fn role_editor<'a>(chat: &'a Chat, role: &'a Role, color: Option<u32>, permissions: Permissions) -> Element<'a, Msg> {
    let c = theme::c();
    let me = chat.standing(role.guild_id);
    let mut editor = Column::new().spacing(12);
    if !role.is_everyone() {
        let swatch = |choice: Option<u32>| {
            let fill = choice.map_or(c.muted, theme::role_color);
            let selected = choice == color;
            button(container(text(if choice.is_none() { "∅" } else { "" }).size(12).color(c.on_fill)).center(22))
                .padding(if selected { 2 } else { 4 })
                .style(move |_, _| button::Style {
                    background: Some(fill.into()),
                    border: iced::Border {
                        color: if selected { c.bright } else { fill },
                        width: 2.0,
                        radius: 14.0.into(),
                    },
                    ..button::Style::default()
                })
                .on_press(Msg::ModalRoleColor(choice))
        };
        let swatches = std::iter::once(None).chain(theme::ROLE_COLORS.iter().copied().map(Some));
        editor = editor.push(Row::with_children(swatches.map(|choice| swatch(choice).into())).spacing(6).wrap());
    }
    let mut boxes = Column::new().spacing(8);
    for (permission, label, hint) in Permissions::LIST {
        let on = permissions.contains(permission);
        let toggle = me.can(permission).then_some(move |on| Msg::ModalPermission(permission, on));
        boxes = boxes.push(
            row![
                checkbox(on).on_toggle_maybe(toggle),
                column![text(label).size(14).color(c.text), text(hint).size(12).color(c.muted)].spacing(2).width(Fill),
            ]
            .spacing(10),
        );
    }
    // Room on the right for the scrollbar.
    editor
        .push(
            container(scrollable(boxes.padding(iced::Padding { right: 14.0, ..iced::Padding::ZERO }))).max_height(300),
        )
        .into()
}

/// The banned users of a guild, each with a button to lift the ban.
fn bans_content<'a>(guild_id: Snowflake, bans: Option<&'a [Ban]>, modal: &'a Modal) -> Element<'a, Msg> {
    let c = theme::c();
    let body: Element<'a, Msg> = match (bans, &modal.error) {
        (_, Some(error)) => text(error).color(c.red).into(),
        (None, None) => text("Chargement…").color(c.muted).into(),
        (Some([]), None) => text("Personne n'est banni de ce serveur.").color(c.muted).into(),
        (Some(list), None) => {
            let mut rows = Column::new().spacing(8);
            for ban in list {
                let who = column![text(&ban.user.username).font(theme::SEMIBOLD).color(c.bright)]
                    .push(ban.reason.as_deref().map(|reason| text(reason).size(13).color(c.muted)))
                    .push(text(format!("Banni : {}", message_time(ban.created_at))).size(12).color(c.muted))
                    .spacing(2)
                    .width(Fill);
                rows = rows.push(
                    container(
                        row![
                            who,
                            button(text("Lever le bannissement").size(13))
                                .padding([6, 10])
                                .style(theme::ghost)
                                .on_press(Msg::Unban { guild_id, user_id: ban.user.id }),
                        ]
                        .spacing(8)
                        .align_y(iced::Center),
                    )
                    .padding(10)
                    .width(Fill)
                    .style(theme::rounded_fill(c.chat, 6.0)),
                );
            }
            container(scrollable(rows)).max_height(420).into()
        }
    };
    column![
        text("Membres bannis").size(20).font(theme::BOLD).color(c.bright),
        body,
        row![
            space::horizontal(),
            button(text("Fermer")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal)
        ],
    ]
    .spacing(16)
    .into()
}

fn invite_content<'a>(invite: Option<&'a plume_lite_protocol::model::Invite>, modal: &'a Modal) -> Element<'a, Msg> {
    let body: Element<'a, Msg> = match (invite, &modal.error) {
        (_, Some(error)) => text(error).color(theme::c().red).into(),
        (None, None) => text("Génération du lien…").color(theme::c().muted).into(),
        (Some(invite), None) => column![
            text("Envoyez ce code à vos amis (valable 7 jours) :").color(theme::c().muted),
            container(
                row![
                    text(&invite.code).size(18).font(theme::BOLD).color(theme::c().bright).width(Fill),
                    button(text("Copier").font(theme::SEMIBOLD))
                        .padding([8, 14])
                        .style(theme::primary)
                        .on_press(Msg::Copy(invite.code.clone())),
                ]
                .align_y(iced::Center),
            )
            .padding([8, 12])
            .style(theme::rounded_fill(theme::c().rail, 6.0)),
            text(format!("{} membre(s) dans {}", invite.member_count, invite.guild.name))
                .size(12)
                .color(theme::c().muted),
        ]
        .spacing(12)
        .into(),
    };
    column![
        text("Inviter des amis").size(20).font(theme::BOLD).color(theme::c().bright),
        body,
        row![
            space::horizontal(),
            button(text("Terminé")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal)
        ],
    ]
    .spacing(16)
    .into()
}

/// What we can do to someone in voice, by permission: mute them, take them out, move them.
fn voice_member_content<'a>(chat: &'a Chat, guild_id: Snowflake, user_id: Snowflake) -> Element<'a, Msg> {
    let name = chat.store.user_name(guild_id, user_id).unwrap_or_else(|| "Ce membre".to_owned());
    let state = chat.store.voice_state(user_id).filter(|s| s.guild_id == guild_id);
    let server_muted = state.is_some_and(|s| s.server_mute);
    let (may_mute, may_move) =
        (chat.can(guild_id, Permissions::MUTE_MEMBERS), chat.can(guild_id, Permissions::MOVE_MEMBERS));
    let mute = if server_muted {
        button(text("Lui rendre la parole").font(theme::SEMIBOLD))
            .padding([10, 16])
            .style(theme::primary)
            .on_press(Msg::ServerMute { guild_id, user_id, muted: false })
    } else {
        button(text("Couper son micro").font(theme::SEMIBOLD))
            .padding([10, 16])
            .style(theme::primary)
            .on_press(Msg::ServerMute { guild_id, user_id, muted: true })
    };
    let disconnect = button(text("Déconnecter du vocal").font(theme::SEMIBOLD))
        .padding([10, 16])
        .style(theme::danger)
        .on_press_maybe(state.is_some().then_some(Msg::DisconnectFromVoice { guild_id, user_id }));
    // The guild's other voice channels, to send them to.
    let current = state.and_then(|s| s.channel_id);
    let destinations: Vec<Element<'a, Msg>> = chat
        .store
        .channels_of(guild_id)
        .into_iter()
        .filter(|c| c.kind == ChannelKind::Voice && Some(c.id) != current)
        .map(|c| {
            button(text(format!("🔊 {}", c.name)).size(13))
                .padding([6, 10])
                .style(theme::ghost)
                .on_press(Msg::MoveToVoice { guild_id, user_id, channel_id: c.id })
                .into()
        })
        .collect();
    let move_to = (may_move && current.is_some() && !destinations.is_empty()).then(|| {
        column![
            text("Déplacer vers").size(13).font(theme::SEMIBOLD).color(theme::c().muted),
            Row::with_children(destinations).spacing(6).wrap(),
        ]
        .spacing(6)
    });
    column![
        text(name).size(20).font(theme::BOLD).color(theme::c().bright),
        text(if server_muted {
            "Son micro est coupé par la modération : il entend les autres mais ne peut pas parler."
        } else {
            "Couper son micro l'empêche de parler dans les salons vocaux de ce serveur, jusqu'à ce qu'on le rétablisse."
        })
        .color(theme::c().muted),
        Row::new().push(may_mute.then_some(mute)).push(may_move.then_some(disconnect)).spacing(8),
        move_to,
        row![
            space::horizontal(),
            button(text("Fermer")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal)
        ],
    ]
    .spacing(16)
    .into()
}

/// The pinned messages of a channel; those who manage messages can unpin them.
fn pins_content<'a>(
    chat: &'a Chat,
    channel_id: Snowflake,
    messages: Option<&'a [(Message, Vec<Block>)]>,
    modal: &'a Modal,
) -> Element<'a, Msg> {
    let c = theme::c();
    let owner =
        chat.store.channel(channel_id).is_some_and(|channel| chat.can(channel.guild_id, Permissions::MANAGE_MESSAGES));
    let body: Element<'a, Msg> = match (messages, &modal.error) {
        (_, Some(error)) => text(error).color(c.red).into(),
        (None, None) => text("Chargement…").color(c.muted).into(),
        (Some([]), None) => text("Aucun message épinglé dans ce salon.").color(c.muted).into(),
        (Some(list), None) => {
            let mut pins = Column::new().spacing(10);
            for (message, blocks) in list {
                let ctx = Context {
                    chat,
                    guild_id: message.guild_id,
                    message_id: message.id,
                    everyone: message.mention_everyone,
                    color: None,
                };
                let unpin = owner.then(|| {
                    button(text("Désépingler").size(12))
                        .padding([2, 8])
                        .style(theme::message_action(false))
                        .on_press(Msg::Pin { channel_id, message_id: message.id, pinned: false })
                });
                let header = row![
                    text(&message.author.username)
                        .font(theme::SEMIBOLD)
                        .color(chat.name_color(message.guild_id, message.author.id)),
                    text(message_time(message.created_at)).size(12).color(c.muted),
                    space::horizontal(),
                ]
                .push(unpin)
                .spacing(8)
                .align_y(iced::Center);
                pins = pins.push(
                    container(column![header, markup::view(ctx, blocks, None)].spacing(4))
                        .padding(10)
                        .width(Fill)
                        .style(theme::rounded_fill(c.chat, 6.0)),
                );
            }
            container(scrollable(pins)).max_height(420).into()
        }
    };
    let name = chat.store.channel(channel_id).map_or("ce salon", |channel| channel.name.as_str());
    column![
        text(format!("Messages épinglés de #{name}")).size(20).font(theme::BOLD).color(c.bright),
        body,
        row![
            space::horizontal(),
            button(text("Fermer")).padding([10, 16]).style(theme::ghost).on_press(Msg::CloseModal)
        ],
    ]
    .spacing(16)
    .into()
}

#[cfg(test)]
mod tests {
    use plume_lite_protocol::model::User;

    use super::*;

    #[test]
    fn audit_entries_read_as_sentences() {
        let user = |name: &str| Some(User { id: Snowflake(1), username: name.into() });
        let entry = |action, actor, target, name: Option<&str>| AuditEntry {
            id: Snowflake(1),
            action,
            actor,
            target_user: target,
            target_id: None,
            target_name: name.map(Into::into),
            reason: None,
            changes: vec![],
        };
        let sentence = |e: &AuditEntry| audit_sentence(e).into_iter().map(|(w, _)| w).collect::<String>();
        let ban = entry(AuditAction::MemberBan, user("alice"), user("dave"), None);
        assert_eq!(sentence(&ban), "alice a banni dave");
        assert_eq!(audit_sentence(&ban)[0], ("alice".to_owned(), true));
        let role = entry(AuditAction::MemberRoleAdd, user("alice"), user("bob"), Some("Modération"));
        assert_eq!(sentence(&role), "alice a donné le rôle Modération à bob");
        let gone = entry(AuditAction::MessageDelete, None, None, Some("#général"));
        assert_eq!(sentence(&gone), "Un compte supprimé a supprimé un message de un compte supprimé dans #général");
    }
}
