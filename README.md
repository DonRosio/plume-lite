# Plume Lite

**Une alternative à Discord, la plus simple possible, à héberger soi-même.** Client de bureau en
Rust + [iced](https://iced.rs) (aucun moteur web, un seul processus), serveur Rust (axum) dont la
base de données tient dans un seul fichier SQLite, temps réel par WebSocket.

![Client Plume Lite](docs/screenshot.png)

## Ce que fait Plume Lite

- Comptes : inscription **sur code d'invitation** donné par l'administrateur, connexion, session
  mémorisée sur le poste ; changement de mot de passe et liste des appareils connectés, avec
  déconnexion à distance (fenêtre ⚙ Paramètres).
- Serveurs (« guilds ») : création, invitations par code (usage/expiration limités), départ, exclusion, suppression.
- Salons textuels : création et suppression (propriétaire), noms normalisés.
- **Salons vocaux** : on voit qui est dedans et qui parle ; micro et son coupables, choix du micro et
  des haut-parleurs, sensibilité réglable, suppression du bruit et annulation d'écho, Opus à
  128 kbit/s, sons d'arrivée et de départ ; le propriétaire peut rendre muet, déplacer quelqu'un
  dans un autre salon vocal ou le sortir du vocal. Le son passe par un serveur
  [LiveKit](https://livekit.io) auto-hébergé.
- Messages en temps réel : envoi optimiste, édition, suppression (auteur ou propriétaire), historique paginé au défilement.
- Messages riches : jusqu'à 4 000 caractères sur plusieurs lignes, Markdown à la Discord (gras,
  italique, code, citations, titres, listes, spoilers, liens), réponses, mentions `@nom` avec
  suggestions et `@everyone` pour le propriétaire ; les messages qui vous concernent sont surlignés.
- Présence en ligne/hors ligne, indicateur « est en train d'écrire… », salons non lus, brouillons par salon.
- Reconnexion automatique (backoff exponentiel + gigue) avec resynchronisation complète.
- Thème clair bleu et rose par défaut, thème sombre en un clic (bouton à côté de la déconnexion).
- Client Windows téléchargeable sur la page d'accueil du serveur ; le client prévient quand une
  nouvelle version est disponible.
- Administration par commandes sur le serveur (codes d'inscription, comptes, mots de passe) et
  sauvegarde quotidienne de la base.

## Démarrage rapide

Prérequis : Rust ≥ 1.94 (`rustup`) et Docker (pour le serveur vocal). Sous Linux, le moteur vocal
du client (WebRTC) se compile avec clang 21 ou plus récent et les en-têtes de glib, et ses sons
avec ceux d'ALSA ; sur Ubuntu
24.04 et ses dérivées, clang 21 vient du dépôt officiel de LLVM :

```bash
cd /tmp && wget https://apt.llvm.org/llvm.sh && sudo bash llvm.sh 21
```

```bash
sudo apt install libglib2.0-dev libasound2-dev
```

La base de données est créée au premier démarrage dans `data/plume-lite.db`. La première fois,
copiez la configuration de développement (elle ouvre les inscriptions sans code, ce qu'il ne faut
pas faire en production) :

```bash
cp .env.example .env
```

Le serveur vocal de développement, puis le serveur :

```bash
docker compose up -d livekit
```

```bash
cargo run --release -p plume-lite-server
```

Dans un autre terminal :

```bash
cargo run --release -p plume-lite-client
```

Créez un compte sur l'écran de connexion (serveur par défaut : `http://127.0.0.1:8080`), ou
remplissez le serveur avec des comptes et une conversation de démonstration :

```bash
python3 scripts/seed_demo.py
```

Pour un deuxième compte sur la même machine, utilisez un profil séparé :

```bash
PLUME_LITE_PROFILE=bob cargo run --release -p plume-lite-client
```

Rendu logiciel (sans GPU) : `ICED_BACKEND=tiny-skia`.

Pour essayer le vocal seul devant son écran, un robot sans fenêtre rejoint un salon vocal, bipe
toutes les trois secondes et affiche le volume auquel il entend les autres :

```bash
cargo run -p plume-lite-client --example voice_bot -- http://127.0.0.1:8080 bob motdepasse-demo <id du salon>
```

## Tests

```bash
cargo test --workspace
```

50 tests : réducteurs du client, protocole, générateur d'IDs, jetons LiveKit, hub de diffusion,
rate limiting, et 13 tests **de bout en bout** qui démarrent le vrai serveur sur un port aléatoire avec une base
SQLite jetable par test (ouverte comme en production), puis parlent HTTP et WebSocket comme un
vrai client (authentification, codes d'inscription, commandes d'administration, sauvegardes,
mot de passe et appareils, expiration des sessions et des invitations, permissions, pagination,
temps réel, présence, rate limiting, page de téléchargement, vocal face à un faux LiveKit). Aucune
installation nécessaire. Un test d'appel réel (deux participants, un vrai LiveKit : l'un émet une
tonalité, l'autre doit l'entendre) se lance à part, la pile de développement démarrée :

```bash
cargo test -p plume-lite-client --test voice_call -- --ignored
```

 La CI GitHub
Actions (`.github/workflows/ci.yml`) exécute `fmt`, `clippy -D warnings` et les tests à chaque
push, et chaque lundi `cargo audit` (dépendances vulnérables) et
[gitleaks](https://github.com/gitleaks/gitleaks) sur tout l'historique (secrets commités). Ses
actions sont figées par empreinte SHA, son jeton est en lecture seule.

### Avant de commiter

Un hook refuse le commit si un secret est détecté ou si le code n'est pas formaté. Il demande
[gitleaks](https://github.com/gitleaks/gitleaks#installing) et s'active une fois par clone :

```bash
git config core.hooksPath .githooks
```

## Mesures (build release, Linux x86_64)

| | Plume Lite |
|---|---|
| Binaire du client | 49,5 Mo, un seul fichier (dont le moteur WebRTC du vocal et 2,6 Mo de polices) |
| Binaire du serveur | 6,0 Mo, SQLite inclus |
| Mémoire du client (rendu GPU wgpu/Vulkan) | RSS 238 Mo, dont **~54 Mo de mémoire propre** ; le reste correspond surtout aux bibliothèques partagées du pilote graphique |
| Mémoire du serveur | ~28 Mo (RSS) après le remplissage de démonstration |

Pour comparaison, le client Discord officiel (Electron) embarque Chromium et lance plusieurs
processus ; son empreinte se compte typiquement en centaines de Mo.

## Structure

```
crates/
  protocol/   contrat client ⇄ serveur partagé : IDs, modèles, DTO REST, trames du gateway, validation
  server/     API REST + gateway WebSocket (axum, sqlx, SQLite) ; migrations dans server/migrations
  client/     application de bureau iced (architecture Elm) : app, store, api, gateway, view/ ;
              polices Noto embarquées dans client/fonts (licence SIL OFL)
deploy/       mise en ligne sur le VPS (Docker, Caddy) : mode d'emploi dans deploy/README.md
docs/
  ARCHITECTURE.md   décisions, flux, schéma de données, sécurité, roadmap
  API.md            référence REST et gateway
```

## Configuration du serveur

Variables d'environnement, toutes facultatives (voir `.env.example`) : `DATABASE_PATH`,
`BIND_ADDR`, `SESSION_TTL_DAYS`, `TRUST_PROXY`, `RATE_LIMIT`, `OPEN_REGISTRATION` (développement
uniquement), `BACKUP_DIR`, `BACKUP_KEEP`, `DOWNLOADS_DIR`, `RUST_LOG`. Les migrations sont
embarquées et appliquées au démarrage.

Le même binaire sert à l'administration, par exemple pour créer un code d'inscription :

```bash
cargo run --release -p plume-lite-server -- invite
```

`cargo run --release -p plume-lite-server -- help` liste toutes les commandes.

## Déploiement

Le workflow GitHub **Release**, déclenché par une étiquette `v<version>`, compile le client
Windows, l'intègre à l'image Docker du serveur et publie celle-ci sur le registre de GitHub. La
mise en ligne sur le VPS (Docker, Caddy, nom de domaine, administration, sauvegardes, mises à
jour) est décrite pas à pas dans [deploy/README.md](deploy/README.md).

Pour essayer l'image en local :

```bash
docker compose up -d --build
```

## Démarche

Plume Lite est né sous le nom de Miniscord, un exercice de conception d'une application de chat
complète, de bout en bout, avec un client natif en Rust plutôt qu'Electron. J'ai défini le cahier
des charges : un MVP de type startup, avec l'architecture, le schéma de données, l'API, l'interface
et le code. J'ai développé le projet avec l'assistance de **Claude** (Anthropic), utilisé comme
binôme de programmation.

Je l'ai ensuite réorienté vers un usage réel : une application la plus simple possible, hébergée
sur mon VPS pour mes amis et moi. J'ai établi la liste complète des fonctionnalités et tranché ce
qu'on garde (par exemple, rien d'animé en permanence, pour préserver les performances), choisi
l'identité visuelle, et arbitré les choix techniques : SQLite plutôt que PostgreSQL (aucun serveur
de base à faire tourner sur un VPS partagé), administration par commandes plutôt que par un
panneau web.

Le code a été écrit avec Claude, puis vérifié : 50 tests, dont 13 de bout en bout avec une vraie base
SQLite et un vrai WebSocket, analyse statique stricte, image Docker construite et lancée, et
essais de l'interface avec deux comptes connectés en même temps. Les décisions d'architecture et
leurs alternatives sont documentées dans [ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [API REST et gateway](docs/API.md)
