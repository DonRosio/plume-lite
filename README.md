# Plume Lite

**Une alternative à Discord, la plus simple possible, à héberger soi-même.** Client de bureau en
Rust + [iced](https://iced.rs) (aucun moteur web, un seul processus), serveur Rust (axum) dont la
base de données tient dans un seul fichier SQLite, temps réel par WebSocket.

![Client Plume Lite](docs/screenshot.png)

## Ce que fait le MVP

- Comptes (inscription, connexion, sessions révocables) ; session mémorisée sur le poste.
- Serveurs (« guilds ») : création, invitations par code (usage/expiration limités), départ, exclusion, suppression.
- Salons textuels : création et suppression (propriétaire), noms normalisés.
- Messages en temps réel : envoi optimiste, édition, suppression (auteur ou propriétaire), historique paginé au défilement.
- Présence en ligne/hors ligne, indicateur « est en train d'écrire… », salons non lus, brouillons par salon.
- Reconnexion automatique (backoff exponentiel + gigue) avec resynchronisation complète.
- Thème clair bleu et rose par défaut, thème sombre en un clic (bouton à côté de la déconnexion).

## Démarrage rapide

Prérequis : Rust ≥ 1.94 (`rustup`). Rien d'autre : la base de données est créée au premier
démarrage dans `data/plume-lite.db`.

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

## Tests

```bash
cargo test --workspace
```

34 tests : réducteurs du client, protocole, générateur d'IDs, hub de diffusion, rate limiting, et
6 tests **de bout en bout** qui démarrent le vrai serveur sur un port aléatoire avec une base
SQLite jetable par test (ouverte comme en production), puis parlent HTTP et WebSocket comme un
vrai client (authentification, expiration des sessions et des invitations, permissions,
pagination, temps réel, présence, rate limiting). Aucune installation nécessaire. La CI GitHub
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
| Binaire du client | 20,8 Mo, un seul fichier |
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
  client/     application de bureau iced (architecture Elm) : app, store, api, gateway, view/
docs/
  ARCHITECTURE.md   décisions, flux, schéma de données, sécurité, roadmap
  API.md            référence REST et gateway
```

## Configuration du serveur

Variables d'environnement, toutes facultatives (voir `.env.example`) : `DATABASE_PATH`,
`BIND_ADDR`, `SESSION_TTL_DAYS`, `TRUST_PROXY`, `RATE_LIMIT`, `RUST_LOG`. Les migrations sont
embarquées et appliquées au démarrage.

## Déploiement

```bash
docker compose up -d --build
```

La base de données est dans le volume Docker `data` (fichier `/data/plume-lite.db`). En
production, placez le serveur derrière un reverse proxy TLS ; avec Caddy, le WebSocket est
relayé sans configuration supplémentaire :

```
plume.donrgames.fr {
    reverse_proxy 127.0.0.1:8080
}
```

Puis `TRUST_PROXY=true`, et dans le client, serveur `https://plume.donrgames.fr` (le gateway
passe automatiquement en `wss://`).

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

Le code a été écrit avec Claude, puis vérifié : 34 tests, dont 6 de bout en bout avec une vraie base
SQLite et un vrai WebSocket, analyse statique stricte, image Docker construite et lancée, et
essais de l'interface avec deux comptes connectés en même temps. Les décisions d'architecture et
leurs alternatives sont documentées dans [ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [API REST et gateway](docs/API.md)
