# API Plume Lite v1

Toutes les structures sont définies dans le crate [`plume-lite-protocol`](../crates/protocol/src) :
c'est la source de vérité, ce document en est le résumé.

## Conventions

- **Base :** `https://<hôte>/api/v1` — JSON en UTF-8.
- **Authentification :** `Authorization: Bearer <jeton>` (obtenu à l'inscription ou à la connexion).
- **Identifiants :** Snowflakes 64 bits **sérialisés en chaînes** (`"233558986324967424"`),
  triables chronologiquement ; la date de création est encodée dedans.
- **Dates :** RFC 3339 en UTC.
- **Erreurs :** toujours la même enveloppe ; les clients se basent sur `code`, jamais sur `message`.

```json
{ "code": "rate_limited", "message": "Trop de requêtes, réessayez dans un instant.", "retry_after_ms": 840 }
```

| HTTP | `code` | Signification |
|---|---|---|
| 400 | `bad_request` | Validation (le `message` est affichable tel quel) |
| 401 | `unauthorized` | Jeton absent, invalide ou expiré ; identifiants incorrects |
| 403 | `forbidden` | Action non autorisée : réservée au propriétaire, code d'invitation manquant ou invalide, compte désactivé, mot de passe actuel incorrect |
| 404 | `not_found` | Inexistant **ou** non visible (non-membre) |
| 409 | `conflict` | Nom d'utilisateur déjà pris |
| 429 | `rate_limited` | Limite atteinte ; en-tête `Retry-After` + `retry_after_ms` |
| 500 | `internal` | Erreur serveur (détails uniquement dans les logs) |

**Limites de débit :** 100 requêtes / 5 s / IP ; inscription, connexion et changement de mot de passe 10 / min / IP ;
messages 5 / 5 s / utilisateur ; frappe 5 / 10 s / utilisateur.

## Endpoints

| Méthode | Chemin | Accès | Corps → Réponse |
|---|---|---|---|
| `GET` | `/instance` | public | → `{version, open_registration, client?: {version, download_path}}` |
| `POST` | `/auth/register` | public | `{username, password, invite_code?}` → **201** `{token, user}` ; code obligatoire sauf inscriptions ouvertes (**403** sinon) |
| `POST` | `/auth/login` | public | `{username, password}` → `{token, user}` ; **403** si le compte est désactivé |
| `POST` | `/auth/logout` | connecté | → **204** (révoque ce jeton) |
| `GET` | `/users/@me` | connecté | → `User` |
| `GET` | `/users/@me/guilds` | connecté | → `[FullGuild]` |
| `PUT` | `/users/@me/password` | connecté | `{current_password, new_password}` → **204** ; déconnecte les autres appareils |
| `GET` | `/users/@me/sessions` | connecté | → `[{id, device, created_at, last_seen_at, current}]`, la plus récemment utilisée d'abord |
| `DELETE` | `/users/@me/sessions/{id}` | connecté | → **204** (déconnecte cet appareil) ; **404** si ce n'est pas une de ses sessions |
| `POST` | `/guilds` | connecté | `{name}` → **201** `FullGuild` (avec le salon « général ») |
| `GET` | `/guilds/{guild_id}` | membre | → `FullGuild` |
| `DELETE` | `/guilds/{guild_id}` | propriétaire | → **204** (salons, messages, invitations supprimés) |
| `GET` | `/guilds/{guild_id}/channels` | membre | → `[Channel]` triés par position |
| `POST` | `/guilds/{guild_id}/channels` | propriétaire | `{name}` → **201** `Channel` (nom normalisé) |
| `GET` | `/guilds/{guild_id}/members?after=&limit=` | membre | → `[Member]` triés par id (limit ≤ 1000) |
| `DELETE` | `/guilds/{guild_id}/members/@me` | membre | → **204** quitter (interdit au propriétaire) |
| `DELETE` | `/guilds/{guild_id}/members/{user_id}` | propriétaire | → **204** exclure |
| `POST` | `/guilds/{guild_id}/invites` | membre | `{max_uses?, max_age_secs?}` → **201** `Invite` |
| `GET` | `/invites/{code}` | connecté | → `Invite` (aperçu : serveur + nb de membres) |
| `POST` | `/invites/{code}` | connecté | → `FullGuild` (rejoindre ; idempotent) |
| `DELETE` | `/channels/{channel_id}` | propriétaire | → **204** |
| `POST` | `/channels/{channel_id}/typing` | membre | → **204** (diffuse `TYPING_START`) |
| `GET` | `/channels/{channel_id}/messages?before=&after=&limit=` | membre | → `[Message]` du plus ancien au plus récent (limit ≤ 100, défaut 50) |
| `POST` | `/channels/{channel_id}/messages` | membre | `{content, nonce?}` → **201** `Message` |
| `PATCH` | `/channels/{channel_id}/messages/{message_id}` | auteur | `{content}` → `Message` |
| `DELETE` | `/channels/{channel_id}/messages/{message_id}` | auteur ou propriétaire | → **204** |

Hors API : `GET /health` (vivacité), `GET /health/ready` (base de données joignable), `GET /`
(page d'accueil : comment installer le client) et `GET /download/plume-lite.exe` (le client
Windows, quand le serveur en distribue un).

Le client s'identifie par son `User-Agent` (`plume-lite-desktop/0.3.0 (windows)`), affiché dans la
liste des appareils. Les codes d'inscription ne s'obtiennent pas par l'API : l'administrateur les
crée sur le serveur (`plume-lite-server invite`, voir `deploy/README.md`).

Règles de validation (partagées client/serveur, `protocol::validate`) : nom d'utilisateur
2-32 caractères `[A-Za-z0-9_.]` ; mot de passe 8-128 ; nom de serveur 2-100 ; nom de salon
normalisé (`« Mon Salon »` → `mon-salon`), ≤ 100 ; message 1-2000 caractères après
`trim`, sans caractères de contrôle hors `\n` et `\t`.

### Exemple

```bash
TOKEN=$(curl -s localhost:8080/api/v1/auth/register -H 'content-type: application/json' \
  -d '{"username":"vincent","password":"un-mot-de-passe"}' | jq -r .token)
GUILD=$(curl -s localhost:8080/api/v1/guilds -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"name":"Rust France"}')
CHANNEL=$(echo "$GUILD" | jq -r '.channels[0].id')
curl -s localhost:8080/api/v1/channels/$CHANNEL/messages -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"content":"Bonjour !"}'
```

## Gateway temps réel

`GET /gateway` → WebSocket, trames texte JSON de la forme `{"op": "...", "d": ...}`.

| Sens | `op` | `d` |
|---|---|---|
| serveur → client | `hello` | `{heartbeat_interval_ms}` — envoyé dès l'ouverture |
| client → serveur | `identify` | `{token}` — dans les 10 s |
| serveur → client | `ready` | `{user, guilds: [FullGuild]}` — état complet, point de resynchronisation |
| client → serveur | `heartbeat` | — toutes les `heartbeat_interval_ms` |
| serveur → client | `heartbeat_ack` | — |
| serveur → client | `dispatch` | `{t: "<ÉVÉNEMENT>", d: …}` |

Événements (`t`) : `MESSAGE_CREATE`, `MESSAGE_UPDATE` (`Message`), `MESSAGE_DELETE`
(`{id, channel_id, guild_id}`), `CHANNEL_CREATE` (`Channel`), `CHANNEL_DELETE`
(`{id, guild_id}`), `GUILD_CREATE` (`FullGuild` : vous avez rejoint ou créé un serveur),
`GUILD_DELETE` (`{id}` : supprimé, quitté ou exclu), `GUILD_MEMBER_ADD`
(`{guild_id, member}`), `GUILD_MEMBER_REMOVE` (`{guild_id, user}`), `PRESENCE_UPDATE`
(`{user_id, status: "online" | "offline"}`), `TYPING_START` (`{guild_id, channel_id, user_id}`).

Règles pour les clients :

1. Appliquer chaque événement de façon **idempotente** (il peut arriver deux fois, et la
   réponse REST d'une mutation fait la course avec son propre événement).
2. Ignorer les trames ou événements inconnus (compatibilité avec les serveurs plus récents).
3. Sans `heartbeat_ack` avant le battement suivant : fermer et se reconnecter.
4. Reconnexion avec backoff exponentiel + gigue ; chaque `ready` remplace tout l'état local.

Codes de fermeture : `4002` trame invalide · `4003` identify manquant ou en retard ·
**`4004` jeton refusé ou révoqué, vérifié à chaque battement (ne pas se reconnecter : retour à l'écran de connexion)** ·
`4005` identify en double · `4008` trop de trames / client trop lent · `4009` battement
manquant · `1012` redémarrage du serveur (se reconnecter).
