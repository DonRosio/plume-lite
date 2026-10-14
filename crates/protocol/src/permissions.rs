//! Roles and permissions, computed the same way by the server (which enforces them) and the
//! client (which only shows what one may do).
//!
//! A member's permissions are those of the guild's `@everyone` role (its id is the guild's)
//! plus those of each of their roles; the owner has them all. Roles are ranked by
//! `position` (`@everyone` is 0, the higher the stronger): one only acts on members and roles
//! ranked below one's own highest role, and only hands out permissions one has.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Snowflake;
use crate::model::Role;

/// A set of permissions. The bits are Discord's, for familiarity; serialised as a decimal
/// string like Discord (a 64-bit integer is not safe in every JSON parser).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Permissions(pub u64);

impl Permissions {
    pub const CREATE_INVITE: Self = Self(1 << 0);
    pub const KICK_MEMBERS: Self = Self(1 << 1);
    pub const BAN_MEMBERS: Self = Self(1 << 2);
    pub const MANAGE_CHANNELS: Self = Self(1 << 4);
    pub const MANAGE_GUILD: Self = Self(1 << 5);
    /// See a channel and read it (join it, for a voice channel).
    pub const VIEW_CHANNEL: Self = Self(1 << 10);
    pub const SEND_MESSAGES: Self = Self(1 << 11);
    pub const MANAGE_MESSAGES: Self = Self(1 << 13);
    pub const MENTION_EVERYONE: Self = Self(1 << 17);
    pub const MUTE_MEMBERS: Self = Self(1 << 22);
    pub const MOVE_MEMBERS: Self = Self(1 << 24);
    pub const MANAGE_ROLES: Self = Self(1 << 28);

    pub const NONE: Self = Self(0);
    /// Every permission this version knows (the owner's).
    pub const ALL: Self = Self(
        Self::CREATE_INVITE.0
            | Self::KICK_MEMBERS.0
            | Self::BAN_MEMBERS.0
            | Self::MANAGE_CHANNELS.0
            | Self::MANAGE_GUILD.0
            | Self::VIEW_CHANNEL.0
            | Self::SEND_MESSAGES.0
            | Self::MANAGE_MESSAGES.0
            | Self::MENTION_EVERYONE.0
            | Self::MUTE_MEMBERS.0
            | Self::MOVE_MEMBERS.0
            | Self::MANAGE_ROLES.0,
    );
    /// What `@everyone` may do in a new guild (and in guilds created before roles).
    pub const DEFAULT_EVERYONE: Self = Self(Self::CREATE_INVITE.0 | Self::VIEW_CHANNEL.0 | Self::SEND_MESSAGES.0);
    /// The permissions a channel can allow or deny per role (see [`channel_permissions`]).
    pub const CHANNEL: Self = Self(Self::VIEW_CHANNEL.0 | Self::SEND_MESSAGES.0);

    /// Every permission with its name and what it allows, in the order the client lists them.
    pub const LIST: [(Self, &'static str, &'static str); 12] = [
        (Self::MANAGE_GUILD, "Gérer le serveur", "Renommer le serveur."),
        (Self::MANAGE_CHANNELS, "Gérer les salons", "Créer, modifier, ordonner et supprimer salons et catégories."),
        (Self::MANAGE_ROLES, "Gérer les rôles", "Créer et modifier les rôles placés sous les siens, et les attribuer."),
        (
            Self::VIEW_CHANNEL,
            "Voir les salons",
            "Lire les salons et rejoindre les salons vocaux (sauf réglage du salon).",
        ),
        (Self::SEND_MESSAGES, "Envoyer des messages", "Écrire dans les salons (sauf réglage du salon)."),
        (Self::CREATE_INVITE, "Créer des invitations", "Inviter de nouvelles personnes."),
        (Self::KICK_MEMBERS, "Exclure des membres", "Faire quitter le serveur aux membres placés sous soi."),
        (Self::BAN_MEMBERS, "Bannir des membres", "Bannir et lever les bannissements ; voir la liste des bannis."),
        (Self::MANAGE_MESSAGES, "Gérer les messages", "Supprimer les messages des autres, épingler."),
        (Self::MENTION_EVERYONE, "Mentionner @everyone", "Notifier tous les membres d'un coup."),
        (Self::MUTE_MEMBERS, "Rendre muet en vocal", "Couper le micro des membres placés sous soi."),
        (Self::MOVE_MEMBERS, "Déplacer en vocal", "Déplacer ou déconnecter du vocal les membres placés sous soi."),
    ];

    /// Only the bits this version knows (a newer server's are ignored).
    pub const fn known(bits: u64) -> Self {
        Self(bits & Self::ALL.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for Permissions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Permissions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Number(u64),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Number(bits) => Ok(Self::known(bits)),
            Raw::Text(text) => text.parse().map(Self::known).map_err(serde::de::Error::custom),
        }
    }
}

/// Where a member stands in a guild: what they may do, and how high they rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    pub owner: bool,
    pub permissions: Permissions,
    /// Position of their highest role (0: only `@everyone`).
    pub rank: i32,
}

impl Standing {
    /// `roles`: the guild's roles (`@everyone` included); `member_roles`: the member's.
    pub fn of<'a>(
        guild_id: Snowflake,
        owner: bool,
        roles: impl IntoIterator<Item = &'a Role>,
        member_roles: &[Snowflake],
    ) -> Self {
        let mut standing = Self { owner, permissions: Permissions::NONE, rank: 0 };
        for role in roles {
            if role.id == guild_id || member_roles.contains(&role.id) {
                standing.permissions = standing.permissions.union(role.permissions);
                if role.id != guild_id {
                    standing.rank = standing.rank.max(role.position);
                }
            }
        }
        if owner {
            standing.permissions = Permissions::ALL;
        }
        standing
    }

    pub fn can(&self, permission: Permissions) -> bool {
        self.owner || self.permissions.contains(permission)
    }

    /// May act on that member (exclude, ban, mute, give or take roles): never on the owner,
    /// otherwise only when ranked strictly higher.
    pub fn outranks(&self, other: &Self) -> bool {
        !other.owner && (self.owner || self.rank > other.rank)
    }

    /// May edit, delete or hand out a role at that position.
    pub fn above_role(&self, position: i32) -> bool {
        self.owner || self.rank > position
    }

    /// May give a role these permissions, or change them from `before`: only those one has.
    pub fn may_grant(&self, before: Permissions, after: Permissions) -> bool {
        let changed = Permissions(before.0 ^ after.0);
        self.owner || self.permissions.contains(changed)
    }
}

/// What a channel allows or denies to the members of one role (`@everyone`: the guild's id),
/// over what their guild permissions say. Only [`Permissions::CHANNEL`] bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Overwrite {
    pub role_id: Snowflake,
    #[serde(default)]
    pub allow: Permissions,
    #[serde(default)]
    pub deny: Permissions,
}

/// A member's permissions in a channel: their guild permissions, then the channel's
/// `@everyone` overwrite, then those of their roles (a role allowing beats one denying).
/// Without [`Permissions::VIEW_CHANNEL`] the channel does not exist for them. The owner has
/// everything everywhere.
pub fn channel_permissions(
    guild_id: Snowflake,
    standing: &Standing,
    member_roles: &[Snowflake],
    overwrites: &[Overwrite],
) -> Permissions {
    if standing.owner {
        return Permissions::ALL;
    }
    let mut permissions = standing.permissions;
    if let Some(everyone) = overwrites.iter().find(|o| o.role_id == guild_id) {
        permissions = permissions.difference(everyone.deny).union(everyone.allow);
    }
    let (mut allow, mut deny) = (Permissions::NONE, Permissions::NONE);
    for overwrite in overwrites.iter().filter(|o| member_roles.contains(&o.role_id)) {
        allow = allow.union(overwrite.allow);
        deny = deny.union(overwrite.deny);
    }
    permissions = permissions.difference(deny).union(allow);
    if permissions.contains(Permissions::VIEW_CHANNEL) {
        permissions
    } else {
        permissions.difference(Permissions::CHANNEL)
    }
}

/// A private channel: `@everyone` may not see it.
pub fn is_private(guild_id: Snowflake, overwrites: &[Overwrite]) -> bool {
    overwrites.iter().any(|o| o.role_id == guild_id && o.deny.contains(Permissions::VIEW_CHANNEL))
}

/// Numbers the roles of a guild 1, 2, … from the weakest (`@everyone` stays at 0), after
/// putting `id` (if given) at `position` (from 1; past the top: highest).
pub fn rank_roles(roles: &mut [Role], moved: Option<(Snowflake, u32)>) {
    let mut order: Vec<(i32, Snowflake)> =
        roles.iter().filter(|r| !r.is_everyone()).map(|r| (r.position, r.id)).collect();
    order.sort_unstable();
    let mut order: Vec<Snowflake> = order.into_iter().map(|(_, id)| id).collect();
    if let Some((id, position)) = moved
        && let Some(from) = order.iter().position(|r| *r == id)
    {
        order.remove(from);
        let to = usize::try_from(position.saturating_sub(1)).unwrap_or(usize::MAX).min(order.len());
        order.insert(to, id);
    }
    for role in roles.iter_mut() {
        role.position =
            if role.is_everyone() { 0 } else { (1..).zip(&order).find(|(_, r)| **r == role.id).map_or(0, |(p, _)| p) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUILD: Snowflake = Snowflake(1);

    fn role(id: i64, position: i32, permissions: Permissions) -> Role {
        Role { id: Snowflake(id), guild_id: GUILD, name: format!("r{id}"), color: None, position, permissions }
    }

    #[test]
    fn permissions_add_up_and_rank_follows_the_highest_role() {
        let roles = [
            role(1, 0, Permissions::CREATE_INVITE),
            role(10, 1, Permissions::MANAGE_MESSAGES),
            role(20, 2, Permissions::KICK_MEMBERS.union(Permissions::MANAGE_ROLES)),
        ];
        let member = Standing::of(GUILD, false, &roles, &[Snowflake(10)]);
        assert!(member.can(Permissions::CREATE_INVITE) && member.can(Permissions::MANAGE_MESSAGES));
        assert!(!member.can(Permissions::KICK_MEMBERS));
        assert_eq!(member.rank, 1);
        let moderator = Standing::of(GUILD, false, &roles, &[Snowflake(20), Snowflake(10)]);
        assert_eq!(moderator.rank, 2);
        let nobody = Standing::of(GUILD, false, &roles, &[]);
        assert_eq!((nobody.permissions, nobody.rank), (Permissions::CREATE_INVITE, 0));
        let owner = Standing::of(GUILD, true, &roles, &[]);
        assert!(owner.can(Permissions::BAN_MEMBERS));

        // Hierarchy: strictly higher, never over the owner.
        assert!(moderator.outranks(&member) && !member.outranks(&moderator));
        assert!(!moderator.outranks(&moderator));
        assert!(owner.outranks(&moderator) && !moderator.outranks(&owner) && !owner.outranks(&owner));
        assert!(moderator.above_role(1) && !moderator.above_role(2));

        // Only the permissions one has can be handed out, added or removed.
        let kick = Permissions::KICK_MEMBERS;
        assert!(moderator.may_grant(Permissions::NONE, kick));
        assert!(moderator.may_grant(kick.union(Permissions::BAN_MEMBERS), Permissions::BAN_MEMBERS));
        assert!(!moderator.may_grant(Permissions::NONE, Permissions::BAN_MEMBERS));
        assert!(!moderator.may_grant(Permissions::BAN_MEMBERS, Permissions::NONE));
        assert!(owner.may_grant(Permissions::NONE, Permissions::ALL));
    }

    #[test]
    fn channels_overwrite_guild_permissions() {
        use Permissions as P;
        let base = P::DEFAULT_EVERYONE;
        let member = Standing { owner: false, permissions: base, rank: 0 };
        let (staff, muted) = (Snowflake(10), Snowflake(20));
        let overwrite = |role, allow, deny| Overwrite { role_id: role, allow, deny };
        let can = |roles: &[Snowflake], overwrites: &[Overwrite], p| {
            channel_permissions(GUILD, &member, roles, overwrites).contains(p)
        };

        // No overwrite: the guild permissions.
        assert!(can(&[], &[], P::VIEW_CHANNEL) && can(&[], &[], P::SEND_MESSAGES));
        // Private: hidden from @everyone, shown to staff.
        let private = [overwrite(GUILD, P::NONE, P::VIEW_CHANNEL), overwrite(staff, P::VIEW_CHANNEL, P::NONE)];
        assert!(is_private(GUILD, &private));
        assert!(!can(&[], &private, P::VIEW_CHANNEL));
        assert!(!can(&[], &private, P::SEND_MESSAGES), "cannot write where one cannot see");
        assert!(can(&[staff], &private, P::VIEW_CHANNEL) && can(&[staff], &private, P::SEND_MESSAGES));
        // Read only for one role; another role allowing wins.
        let read_only = [overwrite(muted, P::NONE, P::SEND_MESSAGES)];
        assert!(!is_private(GUILD, &read_only));
        assert!(can(&[muted], &read_only, P::VIEW_CHANNEL) && !can(&[muted], &read_only, P::SEND_MESSAGES));
        let both = [overwrite(muted, P::NONE, P::SEND_MESSAGES), overwrite(staff, P::SEND_MESSAGES, P::NONE)];
        assert!(can(&[muted, staff], &both, P::SEND_MESSAGES));
        // The owner sees everything.
        let owner = Standing { owner: true, permissions: P::NONE, rank: 0 };
        assert!(channel_permissions(GUILD, &owner, &[], &private).contains(P::VIEW_CHANNEL));
        // A guild without "see channels" for @everyone: only allowed roles see.
        let nobody = Standing { owner: false, permissions: P::NONE, rank: 0 };
        let shown = [overwrite(staff, P::VIEW_CHANNEL, P::NONE)];
        assert!(!channel_permissions(GUILD, &nobody, &[], &shown).contains(P::VIEW_CHANNEL));
        assert!(channel_permissions(GUILD, &nobody, &[staff], &shown).contains(P::VIEW_CHANNEL));
    }

    #[test]
    fn roles_are_ranked_above_everyone() {
        let mut roles =
            vec![role(1, 0, Permissions::NONE), role(10, 5, Permissions::NONE), role(20, 9, Permissions::NONE)];
        let ranks = |roles: &[Role]| roles.iter().map(|r| (r.id.0, r.position)).collect::<Vec<_>>();
        rank_roles(&mut roles, None);
        assert_eq!(ranks(&roles), [(1, 0), (10, 1), (20, 2)]);
        let new = role(30, 0, Permissions::NONE); // created: at the bottom, then ranked
        roles.push(Role { position: 0, ..new });
        rank_roles(&mut roles, Some((Snowflake(30), 1)));
        assert_eq!(ranks(&roles), [(1, 0), (10, 2), (20, 3), (30, 1)]);
        rank_roles(&mut roles, Some((Snowflake(30), 99)));
        assert_eq!(ranks(&roles), [(1, 0), (10, 1), (20, 2), (30, 3)]);
        rank_roles(&mut roles, Some((Snowflake(1), 3))); // @everyone does not move
        assert_eq!(roles[0].position, 0);
    }

    #[test]
    fn serialised_as_a_string_and_unknown_bits_are_dropped() {
        let json = serde_json::to_string(&Permissions::MANAGE_ROLES).unwrap();
        assert_eq!(json, r#""268435456""#);
        assert_eq!(serde_json::from_str::<Permissions>(&json).unwrap(), Permissions::MANAGE_ROLES);
        let newer: Permissions = serde_json::from_str(&format!("\"{}\"", (1u64 << 40) | 1)).unwrap();
        assert_eq!(newer, Permissions::CREATE_INVITE);
        assert_eq!(serde_json::from_str::<Permissions>("6").unwrap(), Permissions(6));
        assert!(serde_json::from_str::<Permissions>(r#""abc""#).is_err());
        assert_eq!(Permissions::LIST.iter().fold(Permissions::NONE, |all, (p, ..)| all.union(*p)), Permissions::ALL);
    }
}
