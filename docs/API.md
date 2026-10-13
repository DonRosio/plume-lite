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
| 503 | `unavailable` | Fonction non activée sur ce serveur (le vocal sans LiveKit) |

**Limites de débit :** 100 requêtes / 5 s / IP ; inscription, connexion et changement de mot de passe 10 / min / IP ;
messages 5 / 5 s / utilisateur ; frappe 5 / 10 s / utilisateur.

## Endpoints

| Méthode | Chemin | Accès | Corps → Réponse |
|---|---|---|---|
| `GET` | `/instance` | public | → `{version, open_registration, client?: {version, download_path, downloads: [{platform: "windows" \| "linux", path, size, sha256, signature_path?}]}}` ; `download_path` = la version Windows (clients ≤ 0.5.0) ; `signature_path` = signature minisign du fichier |
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
| `POST` | `/guilds/{guild_id}/channels` | propriétaire | `{name, kind?: "text" \| "voice"}` → **201** `Channel` (nom normalisé) |
| `GET` | `/guilds/{guild_id}/members?after=&limit=` | membre | → `[Member]` triés par id (limit ≤ 1000) |
| `DELETE` | `/guilds/{guild_id}/members/@me` | membre | → **204** quitter (interdit au propriétaire) |
| `DELETE` | `/guilds/{guild_id}/members/{user_id}` | propriétaire | → **204** exclure |
| `POST` | `/guilds/{guild_id}/invites` | membre | `{max_uses?, max_age_secs?}` → **201** `Invite` |
| `GET` | `/invites/{code}` | connecté | → `Invite` (aperçu : serveur + nb de membres) |
| `POST` | `/invites/{code}` | connecté | → `FullGuild` (rejoindre ; idempotent) |
| `DELETE` | `/channels/{channel_id}` | propriétaire | → **204** |
| `POST` | `/channels/{channel_id}/typing` | membre | → **204** (diffuse `TYPING_START`) |
| `PUT` | `/users/@me/notifications/{guild_id ou channel_id}` | membre | `{level: "all" \| "mentions" \| "nothing"}` → **204** ; envoie `NOTIFICATION_SETTING_UPDATE` à tous les appareils de l'utilisateur ; **404** pour un serveur ou un salon inaccessible |
| `DELETE` | `/users/@me/notifications/{guild_id ou channel_id}` | membre | → **204** : retour au défaut (tous les messages ; pour un salon, le réglage de son serveur) |
| `POST` | `/channels/{channel_id}/ack` | membre | `{message_id}` → **204** : lu jusqu'à ce message (jamais en arrière) ; envoie `READ_STATE_UPDATE` à tous les appareils de l'utilisateur ; **404** si le message n'est pas de ce salon. Écrire un message vaut lecture |
| `POST` | `/channels/{channel_id}/voice` | membre | → `{url, token}` : jeton LiveKit (10 min) pour ce seul salon vocal ; **400** pour un salon textuel, **503** si le vocal n'est pas activé |
| `PATCH` | `/users/@me/voice` | connecté | `{self_mute, self_deaf}` → **204** ; diffuse `VOICE_STATE_UPDATE` si l'on est en vocal |
| `PUT` | `/guilds/{guild_id}/voice/{user_id}/mute` | propriétaire | `{muted}` → **204** ; le membre ne peut plus parler (LiveKit lui retire le droit de publier) |
| `POST` | `/guilds/{guild_id}/voice/{user_id}/disconnect` | propriétaire | → **204** ; **404** s'il n'est pas en vocal |
| `POST` | `/guilds/{guild_id}/voice/{user_id}/move` | propriétaire | `{channel_id}` → **204** ; envoie `VOICE_MOVE` au seul membre déplacé ; **400** pour un salon textuel, **404** s'il n'est pas en vocal ou si le salon n'est pas de ce serveur |
| `POST` | `/voice/webhook` | LiveKit | Arrivées et départs ; signé avec le secret partagé (`Authorization` : JWT dont `sha256` = empreinte du corps) |
| `GET` | `/channels/{channel_id}/messages?before=&after=&limit=` | membre | → `[Message]` du plus ancien au plus récent (limit ≤ 100, défaut 50) |
| `POST` | `/channels/{channel_id}/messages` | membre | `{content, reply_to?, nonce?}` → **201** `Message` ; `reply_to` : un message du même salon (**400** sinon) |
| `PATCH` | `/channels/{channel_id}/messages/{message_id}` | auteur | `{content}` → `Message` |
| `DELETE` | `/channels/{channel_id}/messages/{message_id}` | auteur ou propriétaire | → **204** |
| `PUT` | `/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me` | membre | → **204** (idempotent) ; diffuse `REACTION_ADD` ; **400** si ce n'est pas un emoji ou si le message a déjà 20 emojis différents ; 10 réactions/5 s par utilisateur |
| `DELETE` | `/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me` | membre | → **204** (idempotent) ; diffuse `REACTION_REMOVE` |
| `GET` | `/guilds/{guild_id}/messages/search?q=&channel_id=&author_id=&before=` | membre | → `{total, messages: [Message]}` : les messages des salons textuels qui contiennent chaque mot de `q` (ou un mot qui commence par lui), sans tenir compte des majuscules ni des accents, du plus récent au plus ancien, 25 par page (`before` = page suivante) ; **400** sans mot à chercher ou au-delà de 200 caractères ; 10 recherches/10 s par utilisateur |
| `GET` | `/channels/{channel_id}/pins` | membre | → `[Message]` épinglés, du plus récemment épinglé au plus ancien (50 au plus) |
| `PUT` / `DELETE` | `/channels/{channel_id}/pins/{message_id}` | propriétaire | → **204** (idempotent) ; diffuse `MESSAGE_UPDATE` (`pinned`) ; **400** au-delà de 50 épinglés |

Hors API : `GET /health` (vivacité), `GET /health/ready` (base de données joignable), `GET /`
(page d'accueil : comment installer le client) et `GET /download/plume-lite.exe` (le client
Windows, quand le serveur en distribue un).

Le client s'identifie par son `User-Agent` (`plume-lite-desktop/0.3.0 (windows)`), affiché dans la
liste des appareils. Les codes d'inscription ne s'obtiennent pas par l'API : l'administrateur les
crée sur le serveur (`plume-lite-server invite`, voir `deploy/README.md`).

Règles de validation (partagées client/serveur, `protocol::validate`) : nom d'utilisateur
2-32 caractères `[A-Za-z0-9_.]` ; mot de passe 8-128 ; nom de serveur 2-100 ; nom de salon
normalisé (`« Mon Salon »` → `mon-salon`), ≤ 100 ; message 1-4000 caractères après
`trim`, sans caractères de contrôle hors `\n` et `\t`.

**Messages.** Un `Message` porte `reply_to` quand il répond à un autre :
`{id, author?, excerpt}`, où `excerpt` est le début du message visé (200 caractères) ; `author`
est absent une fois ce message supprimé. `mention_everyone: true` quand le propriétaire du
serveur a écrit `@everyone` (venant d'un autre membre, ce n'est que du texte). `reactions` :
`[{emoji, users}]`, dans l'ordre d'apparition des emojis (un emoji Unicode, encodé dans l'URL) ;
`pinned: true` pour un message épinglé. Le contenu suit un
sous-ensemble du Markdown de Discord, lu de la même façon par le serveur et le client
(`protocol::markup`) :

| Écrit | Rendu |
|---|---|
| `**gras**`, `*italique*` ou `_italique_`, `__souligné__`, `~~barré~~` | mise en forme (combinable) |
| `` `code` ``, ```` ```rust … ``` ```` | code en ligne, bloc de code |
| `> citation`, `>>> ` (tout le reste du message) | citation |
| `# Titre`, `## …`, `### …`, `- élément` | titres, liste |
| `\|\|spoiler\|\|` | masqué jusqu'au clic |
| `https://…` | lien cliquable |
| `<@id>`, `@everyone` | mention (le client affiche `@nom`) |
| `\*` | caractère littéral |

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
(`{user_id, status: "online" | "offline"}`), `TYPING_START` (`{guild_id, channel_id, user_id}`),
`VOICE_STATE_UPDATE` (`{guild_id, user_id, channel_id, self_mute, self_deaf, server_mute}` ;
`channel_id: null` = a quitté le vocal), `VOICE_MOVE` (`{guild_id, channel_id}`, envoyé au seul
membre que le propriétaire déplace : s'il est en vocal dans ce serveur, son client rejoint
`channel_id`), `REACTION_ADD` / `REACTION_REMOVE` (`{guild_id, channel_id, message_id, user_id,
emoji}` : à appliquer comme l'ajout ou le retrait d'un élément d'un ensemble), `READ_STATE_UPDATE`
(`{channel_id, last_read_id, mentions}`, envoyé au seul utilisateur, à ignorer s'il est plus ancien
que l'état connu), `NOTIFICATION_SETTING_UPDATE` (`{target_id, level}`, `level: null` = retour au
défaut ; envoyé au seul utilisateur). `ready` porte aussi `notification_settings`
(`[{target_id, level}]`) et `read_states` : pour chaque salon textuel lu, ou jamais lu
mais non vide (`last_read_id` 0), où l'utilisateur s'est arrêté et combien de messages non lus le
mentionnent (99 au plus) ; chaque salon porte `last_message_id`. Dans `ready` et
`GUILD_CREATE`, chaque serveur porte aussi `voice_states` : qui est dans quel salon vocal.

Le son ne passe pas par ce serveur : le client rejoint la salle LiveKit du salon (nommée d'après
son id) avec le jeton de `POST /channels/{id}/voice`, son identité étant son id d'utilisateur.
LiveKit prévient le serveur des arrivées et départs (`/voice/webhook`), qui les diffuse.

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
