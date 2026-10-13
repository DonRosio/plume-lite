# Architecture de Plume Lite

> Une alternative à Discord la plus simple possible : un client **natif** (Rust + iced, sans
> moteur web) et un serveur Rust (axum) auto-hébergé sur un petit VPS, avec sa base de
> données dans **un seul fichier SQLite**. Pensée pour un groupe d'amis ou une petite
> communauté, pas pour des millions d'utilisateurs.

## 1. Objectifs et contraintes

| Exigence | Cible MVP | Comment on l'atteint |
|---|---|---|
| Empreinte du client | Un seul processus, ~50 Mo de mémoire propre | Rendu natif (iced/wgpu), pas de Chromium, cache borné |
| Latence temps réel | < 100 ms de bout en bout | WebSocket persistant, événements sérialisés une seule fois |
| Simplicité d'exploitation | 1 binaire + 1 fichier de base de données | Un seul processus, SQLite embarqué, migrations embarquées, config 12-factor |
| Sécurité de base | Niveau « production » | Argon2id, jetons opaques hachés, rate limiting, validation partagée |
| Hébergement | Un petit VPS partagé avec d'autres sites (2 vCPU, 4 Go) | Serveur de ~30 Mo de RAM, aucun serveur de base de données à côté |

Pas encore faits (voir la [roadmap](#13-roadmap)) : vidéo et partage d'écran,
messages privés, pièces jointes, rôles, notifications.

## 2. Vue d'ensemble

```mermaid
flowchart LR
    subgraph Desktop["Poste utilisateur"]
        C["Client natif<br/>Rust + iced<br/>(1 processus)"]
    end
    subgraph Edge["Bord"]
        P["Reverse proxy TLS<br/>(Caddy / nginx)"]
    end
    subgraph Backend["Serveur Plume Lite (monolithe modulaire)"]
        R["API REST<br/>/api/v1"]
        G["Gateway WebSocket<br/>/gateway"]
        H["Hub<br/>(diffusion + présence)"]
        R -- "dispatch_*()" --> H
        H -- "file bornée par session" --> G
    end
    DB[("SQLite<br/>(un fichier, WAL)")]
    L["LiveKit<br/>(serveur vocal)"]

    C -- "HTTPS : lectures + mutations" --> P
    C <-- "WSS : événements poussés" --> P
    P --> R
    P <--> G
    R --> DB
    G -- "READY" --> DB
    C <-- "WSS /rtc (via Caddy) + UDP 7882 : le son" --> L
    R -- "jetons, droits, éjections" --> L
    L -- "webhooks : arrivées, départs" --> R
```

**Principe directeur (le même que Discord) :** toutes les mutations passent par REST
(simples à valider, à limiter, à rejouer, à tester), et le WebSocket ne sert qu'à **pousser**
les conséquences aux clients concernés. Le client n'envoie sur le gateway que `identify` et
`heartbeat`.

## 3. Organisation du code

```
plume-lite/
├── Cargo.toml                 # workspace (versions communes, lints, profils)
├── crates/
│   ├── protocol/              # contrat client ⇄ serveur, partagé à la compilation
│   │   └── src/{snowflake,model,http,gateway,validate}.rs
│   ├── server/
│   │   ├── migrations/        # SQL versionné, embarqué dans le binaire
│   │   ├── src/
│   │   │   ├── main.rs        # câblage : config, pool, migrations, arrêt gracieux
│   │   │   ├── lib.rs         # application réutilisée telle quelle par les tests
│   │   │   ├── routes/        # handlers HTTP fins (validation → db → hub)
│   │   │   ├── db/            # SQL par agrégat, sans ORM, sans N+1
│   │   │   ├── gateway/       # session WebSocket + hub de diffusion/présence
│   │   │   ├── auth.rs        # Argon2id, jetons, extracteur AuthUser
│   │   │   ├── admin.rs       # commandes d'administration (invite, users, reset-password…)
│   │   │   ├── backup.rs      # sauvegardes quotidiennes (VACUUM INTO)
│   │   │   ├── voice/         # vocal : jetons LiveKit (JWT HS256), qui est où, appels à LiveKit
│   │   │   ├── ids.rs         # générateur Snowflake sans verrou
│   │   │   ├── rate_limit.rs  # token buckets
│   │   │   └── error.rs       # enveloppe d'erreur unique
│   │   └── tests/api.rs       # tests de bout en bout (HTTP + WebSocket réels)
│   └── client/
│       └── src/
│           ├── main.rs        # démarrage iced
│           ├── app.rs         # machine à états (architecture Elm)
│           ├── store.rs       # cache normalisé + réducteurs purs (testés)
│           ├── api.rs         # client REST typé
│           ├── gateway.rs     # connexion temps réel (Subscription iced)
│           ├── voice.rs       # appels : salle LiveKit, micro et haut-parleurs (Subscription iced)
│           ├── session.rs     # session (fichier 0600) et préférences persistées
│           ├── theme.rs       # thèmes clair (par défaut) et sombre, styles
│           └── view/          # rendu pur : login, sidebar, chat, members, modal, settings
├── deploy/                    # mise en ligne sur le VPS : compose, Caddy, mode d'emploi
└── docs/
```

Le crate **`protocol`** est la pièce maîtresse : modèles, DTO REST, trames du gateway,
codes d'erreur et règles de validation sont définis **une seule fois**. Un changement de
format qui casse le client casse la compilation, pas la production.

## 4. Décisions d'architecture

| # | Décision | Alternatives écartées | Raison |
|---|---|---|---|
| 1 | **Rust de bout en bout** + crate `protocol` partagé | TS côté serveur | Un seul langage, contrat vérifié par le compilateur, binaires sobres |
| 2 | Client **iced 0.14** (architecture Elm, rendu wgpu avec repli logiciel tiny-skia) | Tauri (WebView), egui, Slint, gpui | Natif sans WebView ; modèle Elm prévisible et testable ; utilisé en production (COSMIC) ; licence MIT |
| 3 | Serveur **axum 0.8** sur tokio | actix-web | Écosystème tower (middlewares), WebSocket intégré, ergonomie |
| 4 | **SQLite** (WAL) + **sqlx** (SQL explicite) | PostgreSQL, ORM, MongoDB | Aucun serveur de base à faire tourner, sécuriser ou mettre à jour ; quelques Mo de RAM sur un VPS partagé ; sauvegarde = un fichier ; transactions, clés étrangères, tables `STRICT`, index composites. Un seul écrivain à la fois, soit des milliers d'écritures par seconde : largement assez pour une instance entre amis |
| 5 | **IDs Snowflake** (64 bits, triables par temps) | UUID v4, auto-incrément | Pagination par curseur gratuite, date de création encodée dans l'id, générés sans aller-retour en base |
| 6 | **Jetons opaques** (256 bits, seul le SHA-256 est stocké) | JWT | Révocation instantanée (logout = `DELETE`), pas de secret de signature à faire tourner ; coût = 1 lookup par clé primaire |
| 7 | **REST pour écrire, WebSocket pour diffuser** | Tout en WebSocket | Rate limiting, codes HTTP, idempotence et tests triviaux |
| 8 | **Un seul processus** (modules séparés à l'intérieur) | Microservices | Un binaire à déployer et à surveiller ; le code reste découpé par responsabilité |
| 9 | **Hub en mémoire** derrière une interface `dispatch_*` | Redis, NATS | Une seule instance par conception : un bus de messages n'apporterait rien ; un serveur Rust tient sans peine des milliers de WebSockets |
| 10 | **Envoi optimiste** avec `nonce` | Attendre la réponse | Perception de latence nulle ; réconciliation déterministe |
| 11 | **READY = point de resynchronisation** | Reprise avec numéros de séquence | Plus simple et robuste pour un MVP ; la reprise (`resume`) est sur la roadmap |
| 12 | **LiveKit** auto-hébergé + son SDK Rust officiel pour le vocal | WebRTC en Rust pur (str0m), Opus sur UDP fait maison, Mumble | Serveur de relais éprouvé (gestion du débit, et la vidéo et le partage d'écran plus tard) ; le moteur audio de Chrome (annulation d'écho, suppression de bruit, choix des périphériques) ; un conteneur de plus, ~100 Mo. Coût : un client plus lourd (~46 Mo) et clang 21 pour le compiler sous Linux |

## 5. Flux principaux

### 5.1 Connexion au gateway

```mermaid
sequenceDiagram
    participant C as Client
    participant G as Gateway
    participant H as Hub
    participant DB as SQLite
    C->>G: Upgrade WebSocket /gateway
    G-->>C: hello {heartbeat_interval_ms: 30000}
    C->>G: identify {token}
    G->>DB: session valide ? (sha256(token))
    G->>DB: serveurs + salons + nb de membres (3 requêtes)
    G->>H: connect(user, guilds) — les événements sont mis en file dès maintenant
    G-->>C: ready {user, guilds, read_states}
    G->>H: PRESENCE_UPDATE online (si 1re session)
    loop toutes les 30 s
        C->>G: heartbeat
        G-->>C: heartbeat_ack
    end
    Note over C,G: Pas d'ack avant le battement suivant → le client coupe et se reconnecte<br/>(backoff exponentiel 1→30 s + gigue). Pas de battement pendant 45 s → le serveur ferme (4009).
```

### 5.2 Envoi d'un message

```mermaid
sequenceDiagram
    participant A as Client d'Alice
    participant API as API REST
    participant DB as SQLite
    participant H as Hub
    participant B as Client de Bob
    A->>A: affiche une copie « en cours » (nonce = n1)
    A->>API: POST /channels/{id}/messages {content, nonce: n1}
    API->>API: rate limit (5/5 s), validation, membre du serveur ?
    API->>DB: INSERT (id = Snowflake)
    API->>H: MESSAGE_CREATE (sérialisé une seule fois)
    H-->>B: MESSAGE_CREATE
    H-->>A: MESSAGE_CREATE (nonce n1 → remplace la copie)
    API-->>A: 201 Message (idempotent avec l'événement)
```

Tous les réducteurs du client sont **idempotents** (insertion triée par id, dédoublonnage) :
peu importe l'ordre d'arrivée de la réponse REST et de l'événement, ou un événement
reçu deux fois autour d'une reconnexion.

## 6. Modèle de données

```mermaid
erDiagram
    users ||--o{ sessions : "possède"
    users ||--o{ guild_members : "est membre"
    guilds ||--o{ guild_members : "contient"
    users ||--o{ guilds : "possède (owner_id)"
    guilds ||--o{ channels : "contient"
    channels ||--o{ messages : "contient"
    users ||--o{ messages : "écrit"
    messages ||--o{ message_mentions : "mentionne"
    messages ||--o{ reactions : "reçoit"
    guilds ||--o{ invites : "a"
    users {
        integer id PK "Snowflake"
        text username "unique, insensible à la casse"
        text password_hash "Argon2id (PHC)"
        integer created_at
    }
    sessions {
        blob token_hash PK "SHA-256 du jeton"
        integer user_id FK
        integer expires_at
    }
    guilds {
        integer id PK
        text name
        integer owner_id FK
    }
    guild_members {
        integer guild_id PK
        integer user_id PK
        integer joined_at
    }
    channels {
        integer id PK
        integer guild_id FK
        text name
        integer kind "0 = texte"
        integer position
    }
    messages {
        integer id PK "date de création encodée"
        integer channel_id FK
        integer author_id FK
        text content "1..4000 caractères, Markdown"
        integer edited_at
        integer reply_to_id "message auquel il répond"
        integer mention_everyone "0 ou 1"
        integer pinned_at "épinglé par le propriétaire"
    }
    read_states {
        integer user_id PK
        integer channel_id PK
        integer last_read_id "lu jusqu'à ce message"
    }
    notification_settings {
        integer user_id FK
        integer guild_id FK "ou channel_id"
        integer channel_id FK
        integer level "0 tous, 1 mentions, 2 rien"
    }
    reactions {
        integer message_id PK
        text emoji PK "emoji Unicode"
        integer user_id PK
        integer created_at
    }
    message_mentions {
        integer message_id PK
        integer user_id PK "membre mentionné"
    }
    invites {
        text code PK "8 caractères [A-Za-z0-9]"
        integer guild_id FK
        integer uses
        integer max_uses "NULL = illimité"
        integer expires_at "Unix (s), NULL = jamais"
    }
```

Index choisis pour les requêtes réellement exécutées :

| Requête chaude | Index | Coût |
|---|---|---|
| Page de messages avant/après X | `messages (channel_id, id DESC)` | 1 parcours d'intervalle, quelle que soit la taille de l'historique |
| Serveurs d'un utilisateur (READY) | `guild_members (user_id)` | O(nb de serveurs) |
| Membres d'un serveur (pagination par clé) | PK `(guild_id, user_id)` | Pas d'`OFFSET`, coût constant par page |
| Authentification | PK `sessions (token_hash)` | 1 lookup |
| Connexion par nom | `username UNIQUE COLLATE NOCASE` | 1 lookup, insensible à la casse |

Garde-fous en base (défense en profondeur) : tables `STRICT` (une valeur du mauvais type est
refusée), `CHECK` sur les longueurs, clés étrangères en cascade, `max_uses > 0`. Les dates
sont des timestamps Unix en secondes, comparés à `unixepoch()`. Le redeem d'une invitation est un `UPDATE … WHERE uses < max_uses
RETURNING` : atomique, donc sans course.

## 7. Le serveur

- **Couches :** `routes` (HTTP fin : extraction → validation → contrôle d'accès → `db` →
  `hub`) ; `db` (SQL par agrégat) ; `gateway` (sessions) ; `hub` (routage des événements).
- **Contrôle d'accès :** une seule requête charge un salon **et** vérifie l'appartenance
  (`db::channels::access`). Un non-membre reçoit **404** (et non 403) : impossible de sonder
  l'existence d'un serveur ou d'un salon.
- **Hub :** ne connaît que les utilisateurs **en ligne** (`user → sessions + serveurs`,
  `serveur → membres en ligne`). Publier coûte O(membres en ligne). Chaque session a une
  file bornée (512) ; un client trop lent est déconnecté (code 4008) puis se resynchronise,
  au lieu de faire gonfler la mémoire du serveur.
- **Arrêt gracieux :** SIGTERM → plus de nouvelles connexions, les sessions gateway ferment
  avec 1012 (« redémarrage ») et les clients se reconnectent avec backoff.
- **Tâches de fond :** purge des buckets de rate limiting (1 min) ; toutes les heures, purge des
  sessions expirées et des codes d'inscription épuisés, et sauvegarde de la base si la dernière
  date de plus de 23 h (les 7 dernières sont gardées).
- **Sessions révoquées :** la REST les refuse aussitôt ; la connexion temps réel revérifie son
  jeton à chaque battement et se ferme (code 4004) dans les 30 s.

## 8. Le client

Architecture **Elm** (celle d'iced) : `état → Msg → update → (nouvel état, Tasks) → view`.

- `App` est une machine à états : `Restoring` (validation du jeton enregistré) → `Login` →
  `Chat`. Une session invalide (401 ou code gateway 4004) renvoie à `Login`.
- `Store` est un **cache normalisé** (serveurs, salons, chronologies, membres, présence,
  frappe, non-lus) alimenté uniquement par des **réducteurs purs**, testés unitairement.
- `gateway.rs` expose la connexion comme une `Subscription` iced indexée par
  `(url, jeton)` : se déconnecter la détruit, changer de compte en crée une nouvelle.
- **Sobriété mémoire :** en quittant un salon, sa chronologie est tronquée à 100 messages ;
  les messages des salons jamais ouverts ne sont pas conservés ; l'historique se charge par
  pages de 50 au défilement.
- **Non-lus :** le serveur retient où chacun s'est arrêté dans chaque salon (`read_states`) et
  le dit à tous ses appareils. Le salon affiché dans une fenêtre active est lu ; le client
  l'envoie une seconde plus tard (une rafale de messages ne coûte qu'une requête). Un salon est
  non lu quand son dernier message est plus récent ; les mentions non lues sont comptées par
  le serveur au `READY`, puis par le client (un même événement reçu deux fois ne compte qu'une
  fois). Le titre de la fenêtre les reprend : « (3) » ou « • ».
- **UX :** envoi optimiste avec relance/abandon en cas d'échec, regroupement des messages
  par auteur (fenêtre de 7 min), séparateurs de jour, brouillons par salon, indicateur de
  frappe (anti-rebond 8 s), édition en ligne (Entrée/Échap), présence en direct.
- **Messages riches :** le Markdown (`protocol::markup`) est analysé une fois par message et
  gardé avec la chronologie, pas à chaque image. Saisie sur plusieurs lignes (Entrée envoie,
  Maj+Entrée va à la ligne) ; en tapant `@`, les membres du serveur sont proposés, et
  `@nom` devient `<@id>` à l'envoi (l'inverse en modification), jamais dans le code. Un
  message qui nous mentionne, ou qui répond à l'un des nôtres, est surligné en rose.
- **Réactions :** chaque réaction est un ensemble d'utilisateurs (pas un compteur) : un
  `REACTION_ADD` reçu deux fois ne compte qu'une fois, et le survol montre qui a réagi. Le clic
  s'affiche aussitôt et l'erreur éventuelle l'annule. Choix parmi 32 emojis courants (les emojis
  personnalisés viendront avec la v0.7).
- **Polices embarquées :** Noto Sans (normal, italique, gras, gras italique) et Noto Sans Mono
  (~2,6 Mo, licence OFL dans `crates/client/fonts/`). La police par défaut d'iced, Open Sans,
  est rarement installée : sans elle, l'italique et le code retombaient sur une police de
  secours, différente selon le système. cosmic-text 0.15 (le moteur de texte d'iced) met en
  forme un mot tout en ASCII avec la police de sa première lettre : là où un style commence
  ou finit au milieu d'un mot (`l'*italique*`), le client insère un « word joiner » invisible
  qui lui fait suivre chaque changement de police.
- **Arbre de widgets stable :** une colonne iced ignore ses enfants vides, et un widget qui
  change de place perd son état (focus, défilement). Les parties optionnelles placées avant
  un champ de saisie (suggestions, réponse, bandeau de reconnexion) sont donc enveloppées dans
  des conteneurs toujours présents.

## 8 bis. Le vocal

Le son ne passe jamais par le serveur Plume Lite : chaque salon vocal est une salle LiveKit,
nommée d'après l'id du salon, et l'identité de chacun y est son id d'utilisateur.

1. **Rejoindre :** le client demande `POST /channels/{id}/voice`. Le serveur vérifie
   l'appartenance au serveur (404 sinon) et le type du salon, puis signe un jeton LiveKit
   (JWT HS256, secret partagé avec LiveKit) valable 10 minutes pour cette seule salle. Un membre
   rendu muet par le propriétaire reçoit un jeton sans droit de publier.
2. **Qui est où :** LiveKit prévient le serveur de chaque arrivée et départ (webhook signé ; le
   jeton porte l'empreinte SHA-256 du corps). Le serveur revérifie l'appartenance (sinon il éjecte
   l'intrus), garde l'état en mémoire et le diffuse (`VOICE_STATE_UPDATE`) ; `READY` et
   `GUILD_CREATE` le contiennent. Après un redémarrage, il redemande à LiveKit qui est dans
   quelle salle.
3. **Micro et son :** le client les applique localement et les signale (`PATCH /users/@me/voice`)
   pour que les autres voient « muet » ou « son coupé ». Couper le son désabonne de toutes les
   pistes : on n'entend plus rien et on ne reçoit plus rien.
4. **Modération :** le propriétaire peut rendre muet (LiveKit retire le droit de publier ; le client
   republie le micro quand il le rend), sortir quelqu'un du vocal ou le déplacer dans un autre
   salon vocal. LiveKit auto-hébergé ne sait pas déplacer quelqu'un d'une salle à l'autre
   (`MoveParticipant` est réservé à son offre hébergée) : le serveur envoie `VOICE_MOVE` au seul
   membre déplacé, dont le client change de salle comme s'il avait cliqué. Quitter le serveur, en
   être exclu, supprimer le salon ou le serveur fait aussi sortir du vocal ; un compte désactivé
   est éjecté de toutes les salles.

**Côté client :** le micro et les haut-parleurs passent par le module audio de WebRTC
(`PlatformAudio`), avec son annulation d'écho et sa suppression de bruit. L'activation à la voix
coupe la piste quand le niveau du micro reste sous le seuil choisi (avec 400 ms de maintien) ; si
le système ne fournit pas ce niveau, le micro reste simplement ouvert. Opus à 128 kbit/s, rien
envoyé pendant les silences (DTX), redondance contre les pertes (RED).

Les sons d'arrivée et de départ (deux notes montantes ou descendantes) sont synthétisés par le
client et joués par `cpal` sur un flux de sortie ouvert le temps du son (le module audio de WebRTC
ne joue que l'appel) : sur la sortie choisie quand le système la trouve sous le même nom, sinon
sur celle par défaut. Aucun son quand on a coupé le sien.

## 9. Sécurité

| Menace | Mesure |
|---|---|
| Vol de base de données | Mots de passe en Argon2id (paramètres OWASP), jetons stockés uniquement en SHA-256 |
| Bourrage d'identifiants | 10 tentatives/min/IP, coût Argon2, temps de réponse égalisé (utilisateur inconnu = vérification factice) |
| Déni de service par hachage | Sémaphore : au plus N hachages Argon2 simultanés (N = nb de cœurs) |
| Flood | Token buckets : 100 req/5 s/IP, 5 messages/5 s/utilisateur, frappe 5/10 s ; WebSocket : 60 trames/min, trames ≤ 4 Kio, identify en 10 s |
| Charges utiles abusives | Corps ≤ 64 Kio, timeout 15 s, validation partagée (longueurs, caractères de contrôle, octet NUL) |
| Énumération | 404 identique pour « inexistant » et « non autorisé » |
| Usurpation d'IP | `X-Forwarded-For` ignoré sauf `TRUST_PROXY=true` (et seule l'entrée la plus à droite est retenue) |
| Fuite d'informations | Erreurs internes journalisées côté serveur, message générique côté client |
| Jeton sur le poste client | Fichier de session en mode 0600 (trousseau du système sur la roadmap) |
| Inscriptions d'inconnus | Inscriptions fermées : un code créé par l'administrateur sur le serveur (nombre d'utilisations et durée limités), consommé atomiquement avec la création du compte |
| Appareil perdu ou session volée | Liste des appareils connectés et déconnexion à distance ; changer son mot de passe déconnecte tous les autres appareils |
| Compte compromis | L'administrateur réinitialise le mot de passe ou désactive le compte ; toutes ses sessions sont fermées |
| Administration exposée | Aucune interface d'administration sur Internet : des commandes à lancer sur le serveur (SSH) |
| Perte de données | Sauvegarde quotidienne cohérente (`VACUUM INTO`) pendant que le serveur tourne, 7 gardées |
| Écoute d'un salon vocal sans y avoir droit | Jetons LiveKit signés, valables 10 min pour une seule salle, délivrés aux seuls membres ; une identité qui n'est plus membre est éjectée dès son arrivée |
| Faux événements vocaux | Webhooks de LiveKit signés avec le secret partagé, empreinte du corps vérifiée |
| API d'administration de LiveKit | Jamais exposée : Caddy ne relaie que la signalisation (`/rtc`) ; le serveur l'appelle sur le réseau interne |
| Composant tiers altéré à la compilation | Actions GitHub figées par SHA ; outils téléchargés (gitleaks, libwebrtc précompilé du moteur vocal) vérifiés par SHA-256 figé dans le dépôt (`scripts/libwebrtc.json`) ; clé du dépôt LLVM vérifiée par son empreinte ; `cargo audit` chaque semaine |
| Client modifié distribué (serveur piraté, fichier altéré en route) | Clients compilés et signés par le workflow Release (minisign, clé secrète dans les secrets GitHub, jamais sur le VPS) ; chaque signature est revérifiée avec la clé publique du dépôt avant publication ; la page de téléchargement affiche SHA-256 et signature ; le client vérifie empreinte et signature de ses mises à jour avec la clé publique qu'il embarque avant de les garder |

TLS est terminé par le reverse proxy (Caddy obtient et renouvelle les certificats tout seul) ;
le client gère `https://` et `wss://` via rustls.

## 10. Une seule instance, par choix

Plume Lite tourne sur **une seule machine** : la base SQLite, le hub de diffusion, la
présence et les limites de débit vivent dans le processus du serveur. C'est ce qui le rend
simple à installer, à sauvegarder et à faire tourner à côté d'autres sites sur un petit VPS.

- **Base de données :** mode WAL (les lectures continuent pendant une écriture),
  `synchronous = NORMAL`, clés étrangères actives, attente de 5 s si la base est occupée.
  Les transactions d'écriture commencent par `BEGIN IMMEDIATE` (verrou pris d'emblée, pas
  d'échec quand une lecture devient une écriture). À l'arrêt, le journal est replié dans
  le fichier principal.
- **Capacité :** un serveur de cette taille supporte sans effort des centaines d'utilisateurs
  connectés ; la limite pratique sera la bande passante du VPS (partage d'écran) et le
  disque (fichiers), pas la base.
- **Si un jour il fallait plusieurs machines :** les handlers ne parlent qu'à
  `hub.dispatch_*` (un bus Redis/NATS pourrait s'y brancher), le SQL est explicite et
  regroupé dans `db/` (portable vers PostgreSQL), l'API est versionnée (`/api/v1`) et les
  formats tolèrent les évolutions (variantes inconnues ignorées par les anciens clients).

## 11. Observabilité

- Logs structurés `tracing` ; une ligne d'accès par requête (statut + latence) avec un
  `x-request-id` généré et renvoyé au client.
- Sondes : `GET /health` (vivacité) et `GET /health/ready` (base joignable).
- À ajouter ensuite : métriques Prometheus (connexions gateway, latence de diffusion,
  taille des files), traces OpenTelemetry, Sentry côté client.

## 12. Limites connues du MVP

- Permissions binaires : propriétaire / membre (rôles et permissions par salon : voir la roadmap).
- Pas de reprise de session gateway : chaque reconnexion repart d'un READY complet.
- Une session révoquée garde sa connexion temps réel jusqu'au battement suivant (30 s au plus).
- Liste des membres limitée à 1 000 par serveur côté client.
- Vocal : pas de volume par personne (le SDK ne le permet pas encore avec le module audio de
  WebRTC) ; les « rendu muet » sont oubliés au redémarrage du serveur ; un client antérieur à la
  0.4.1 ignore un déplacement (le propriétaire peut alors le sortir du vocal) ; sous Linux, les
  sons d'arrivée et de départ passent par la sortie par défaut (ALSA ne nomme pas les sorties
  comme WebRTC).
- Pas de sélection du texte des messages (le bouton « Copier » couvre le besoin) ; cliquer sur
  l'extrait d'une réponse ne fait pas encore défiler jusqu'au message visé.
- Pas d'e-mail : un mot de passe oublié est réinitialisé par l'administrateur. Pas encore de 2FA.
- Les sauvegardes restent sur le disque du VPS : les rapatrier de temps en temps (voir
  `deploy/README.md`).

## 13. Roadmap

Proposition de découpage, à partir de la liste de fonctionnalités validée (identifiants A1 à
L7). Règle commune : rien ne s'anime tout seul, et ce qui est lourd (GIF, vidéos, flux
vidéo) ne se lance qu'à la demande.

| Version | Contenu |
|---|---|
| v0.2 (faite) | Renommage en Plume Lite, passage à SQLite, thème clair bleu et rose par défaut et thème sombre (K1) |
| v0.3 Mise en ligne (faite) | Inscription sur invitation (L3), administration par commandes sur le VPS (L4, A13), sauvegardes (L2), déploiement Docker + Caddy sur `plume.donrgames.fr` (L1), client Windows compilé par la CI (K3), changement de mot de passe (A9), appareils connectés (A11), avis de nouvelle version (K7) |
| v0.4 Vocal (faite) | Serveur LiveKit sur le VPS ; salons vocaux (C5, H1-H4, H6, H8, H9), modération : rendre muet, sortir du vocal (H10) |
| v0.4.1 (faite) | Sons d'arrivée et de départ (H11), déplacer quelqu'un d'un salon vocal à un autre (H10) ; volume par personne (H7) reporté : le SDK ne le permet pas encore |
| v0.5.0 Messages riches (faite) | 4 000 caractères (E9), saisie sur plusieurs lignes, Markdown (E5), réponses (E6), mentions et `@everyone` du propriétaire (E7) |
| v0.5.1 (faite) | Réactions (E8), messages épinglés par le propriétaire (E10) ; client Linux distribué avec le client Windows, empreintes SHA-256 et signatures minisign des clients, mises à jour vérifiées par le client, libwebrtc figé par empreinte |
| v0.5.2 | Non-lus mémorisés (E4), notifications (tous les messages par défaut, réglables par serveur et salon) et badge (J1-J4), zone de notification (K2) |
| v0.5.3 | Recherche (E11) |
| v0.6 Rôles et salons | Rôles et permissions (D1-D6), droits par salon simplifiés (D7), salons privés (C6), catégories (C4), renommer et réordonner (C2, C3), bannir (B4), serveur renommable avec icône (B5), transfert de propriété (B6), journal d'audit (B8), bienvenue (B10) |
| v0.7 Fichiers et profils | Soundboard (H12), pièces jointes jusqu'à 2 Go avec reprise (F1-F5), avatar (A3), pseudo (A4), statuts (A5, A6), bio et bannière (A7), emojis (B9), aperçus de liens (E12), stickers (E13), GIF (E14) |
| v0.8 Messages privés | Messages privés, groupes, amis et blocage (G1-G3), appels privés (G4) |
| v0.9 Vidéo | Caméra et partage d'écran 1080p 60 ips avec son (I1-I6) |
| v1.0 | Double authentification (A10), suppression de compte (A12), page d'état (L7), mode compact (K6), chiffrement de bout en bout du vocal (L6, à étudier) |
