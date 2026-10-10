//! Administration commands, typed on the server machine:
//! `plume-lite-server <command>` (with Docker: `docker compose exec server plume-lite-server <command>`).
//!
//! They work on the database directly, next to the running server: nothing to log in to, and
//! nothing reachable from the Internet. Output is in French, for the administrator.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context, bail};
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::SqlitePool;

use crate::config::Config;
use crate::voice::Voice;
use crate::{auth, backup, db};

pub const USAGE: &str = "\
Plume Lite — serveur

Sans argument, démarre le serveur. Commandes d'administration :

  invite [--uses N] [--days N]   crée un code d'inscription (1 utilisation, 7 jours par défaut)
  invites                        liste les codes encore valables
  revoke-invite CODE             supprime un code
  users                          liste les comptes
  reset-password NOM             donne un nouveau mot de passe et ferme toutes ses sessions
  disable NOM                    désactive un compte et ferme toutes ses sessions
  enable NOM                     réactive un compte
  backup                         sauvegarde la base maintenant
  health                         vérifie que le serveur répond (code de sortie 0)
  help                           affiche cette aide";

/// Runs one administration command and returns what to print.
pub async fn run(db: &SqlitePool, config: &Config, args: &[String]) -> anyhow::Result<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["invite", options @ ..] => invite(db, options).await,
        ["invites"] => invites(db).await,
        ["revoke-invite", code] => {
            if !db::registration::delete(db, code).await? {
                bail!("Code introuvable : {code}");
            }
            Ok(format!("Code {code} supprimé."))
        }
        ["users"] => users(db).await,
        ["reset-password", name] => reset_password(db, name).await,
        ["disable", name] => {
            let user = user(db, name).await?;
            let changed = db::users::set_disabled(db, user.id, true).await?;
            let closed = db::users::delete_sessions(db, user.id, None).await?;
            let voice = match kick_from_voice(config, user.id).await {
                Ok(0) => "",
                Ok(_) => " Retiré du vocal.",
                Err(_) => " Impossible de joindre LiveKit pour le retirer du vocal.",
            };
            Ok(if changed {
                format!("Compte {} désactivé ({closed} session(s) fermée(s)).{voice}", user.username)
            } else {
                format!("Le compte {} était déjà désactivé.", user.username)
            })
        }
        ["enable", name] => {
            let user = user(db, name).await?;
            Ok(if db::users::set_disabled(db, user.id, false).await? {
                format!("Compte {} réactivé.", user.username)
            } else {
                format!("Le compte {} était déjà actif.", user.username)
            })
        }
        ["backup"] => {
            let dir = config.backup_dir.as_deref().context("Les sauvegardes sont désactivées (BACKUP_DIR=off).")?;
            let path = backup::create(db, dir).await?;
            let size = tokio::fs::metadata(&path).await?.len();
            Ok(format!("Sauvegarde écrite : {} ({} Ko)", path.display(), size.div_ceil(1024)))
        }
        ["help" | "--help" | "-h"] => Ok(USAGE.to_owned()),
        _ => bail!("Commande inconnue : {}\n\n{USAGE}", args.join(" ")),
    }
}

async fn invite(db: &SqlitePool, options: &[&str]) -> anyhow::Result<String> {
    let (mut uses, mut days) = (1i64, 7i64);
    let mut options = options.iter();
    while let Some(option) = options.next() {
        let value = options.next().and_then(|v| v.parse::<i64>().ok());
        match (*option, value) {
            ("--uses", Some(n)) if (1..=100).contains(&n) => uses = n,
            ("--days", Some(n)) if (1..=30).contains(&n) => days = n,
            _ => bail!("Option invalide : {option} (--uses 1 à 100, --days 1 à 30)"),
        }
    }
    let expires_at = Utc::now() + TimeDelta::days(days);
    let code = loop {
        let code = auth::random_code(10)?;
        if db::registration::create(db, &code, uses, expires_at).await? {
            break code;
        }
    };
    Ok(format!(
        "Code d'inscription : {code}\n\
         Valable jusqu'au {}, pour {uses} inscription(s).\n\
         Dans Plume Lite, la personne choisit « Inscrivez-vous », entre l'adresse du serveur puis ce code.",
        date(expires_at)
    ))
}

async fn invites(db: &SqlitePool) -> anyhow::Result<String> {
    let codes = db::registration::usable(db).await?;
    if codes.is_empty() {
        return Ok("Aucun code d'inscription valable.".to_owned());
    }
    let mut out = String::from("CODE        UTILISÉ  EXPIRE LE");
    for code in codes {
        let _ = write!(out, "\n{:<10}  {:>3}/{:<3}  {}", code.code, code.uses, code.max_uses, date(code.expires_at));
    }
    Ok(out)
}

async fn users(db: &SqlitePool) -> anyhow::Result<String> {
    let accounts = db::users::accounts(db).await?;
    if accounts.is_empty() {
        return Ok("Aucun compte.".to_owned());
    }
    let mut out = format!("{:<32}  {:<10}  {:<17}  APPAREILS  ÉTAT", "NOM", "CRÉÉ LE", "DERNIÈRE ACTIVITÉ");
    for account in accounts {
        let _ = write!(
            out,
            "\n{:<32}  {:<10}  {:<17}  {:>9}  {}",
            account.username,
            account.created_at.format("%d/%m/%Y"),
            account.last_seen_at.map_or_else(|| "—".to_owned(), date),
            account.sessions,
            if account.disabled { "désactivé" } else { "actif" },
        );
    }
    Ok(out)
}

async fn reset_password(db: &SqlitePool, name: &str) -> anyhow::Result<String> {
    let user = user(db, name).await?;
    let password = auth::random_password()?;
    let hash = auth::hash_password(password.clone()).await?;
    db::users::set_password(db, user.id, &hash).await?;
    let closed = db::users::delete_sessions(db, user.id, None).await?;
    Ok(format!(
        "Nouveau mot de passe de {} : {password}\n\
         {closed} session(s) fermée(s). Transmettez-le en privé ; il pourra le changer dans les paramètres.",
        user.username
    ))
}

async fn user(db: &SqlitePool, name: &str) -> anyhow::Result<plume_lite_protocol::model::User> {
    db::users::by_username(db, name).await?.with_context(|| format!("Compte introuvable : {name}"))
}

/// Removes the user from every LiveKit room (this process does not share the running
/// server's memory: it asks LiveKit directly). Returns how many rooms they were in.
async fn kick_from_voice(config: &Config, user_id: plume_lite_protocol::Snowflake) -> anyhow::Result<usize> {
    let voice = Voice::new(config.livekit.clone());
    if !voice.enabled() {
        return Ok(0);
    }
    let identity = user_id.to_string();
    let mut kicked = 0;
    for (room, identities) in voice.rooms().await? {
        if let (true, Ok(room)) = (identities.contains(&identity), room.parse()) {
            voice.remove_participant(room, user_id).await?;
            kicked += 1;
        }
    }
    Ok(kicked)
}

fn date(at: DateTime<Utc>) -> String {
    at.format("%d/%m/%Y %H:%M").to_string()
}

/// `health`: asks the running server for `/health/ready`, for Docker's health check
/// (the image has no curl).
pub fn health(config: &Config) -> anyhow::Result<()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.bind_addr.port()));
    let timeout = Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&addr, timeout).with_context(|| format!("{addr} ne répond pas"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(b"GET /health/ready HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut response = String::new();
    stream.take(1024).read_to_string(&mut response)?;
    let status = response.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("200") {
        bail!("Le serveur répond « {status} »");
    }
    Ok(())
}
