# Putting Plume Lite online

Plume Lite runs on the same OVH VPS as statslol and donrgames.fr, in a Docker container, behind the
server's Caddy, at `https://plume.donrgames.fr`. This folder holds everything that goes on the
server; no address, no password and no key: the repository does not need any.

The commands marked "PC" are run on your computer, from the root of the repository; they reach the
server through its SSH alias (`Host serveur` in `~/.ssh/config`). The others are run on the server,
as `ubuntu` (`sudo` asks for its password).

## What is already in place

The server was prepared for statslol (see `deploy/server/README.md` in the statslol repository): SSH
by key only, a firewall, automatic updates, Docker, and Caddy, which obtains the HTTPS certificates
by itself. Docker is already logged in to GitHub's container registry with a read-only token. The
site and the voice signalling go through Caddy's port 443; only the sound itself uses two more
ports, opened by Docker (7881 over TCP, 7882 over UDP: ports published by Docker bypass the
firewall).

## 1. Publishing a version

Every version is a Git tag `v<version in Cargo.toml>`. The **Release** workflow builds the Windows
and Linux clients, signs them, puts them in the server's image, tries the image as in production,
then publishes it: `ghcr.io/donrosio/plume-lite-server:<version>`.

### The signing key (once)

The clients are signed with [minisign](https://jedisct1.github.io/minisign/). The secret key lives
only in the GitHub repository's secrets; the public key is in the repository (`minisign.pub`) and in
every client, which uses it to check its updates. A version without a signature is not published.

```bash
# PC, at the root of the repository. The secret key goes outside the repository, readable by you only.
sudo apt install minisign
mkdir -p ~/.minisign && chmod 700 ~/.minisign
minisign -G -W -p minisign.pub -s ~/.minisign/plume-lite.key
gh secret set MINISIGN_SECRET_KEY < ~/.minisign/plume-lite.key
```

Keep a copy of `~/.minisign/plume-lite.key` off the PC (USB key, password manager): without it,
the clients already installed can no longer check an update. If it leaks: generate a new one,
replace the secret and `minisign.pub`, publish a version, and tell everyone to download the client
again from the server's page (the old clients will refuse updates signed by the new key).

```bash
# PC
git tag v0.3.0 && git push origin v0.3.0
```

Wait for the Release workflow to be green (the Actions tab on GitHub).

## 2. The domain name

In the OVH customer area, DNS zone of `donrgames.fr`: add an **A** record `plume` pointing to the
server's IPv4 address (the same as `statslol`). As for statslol, no AAAA record. Propagation takes
from a few minutes to an hour:

```bash
# PC
dig +short plume.donrgames.fr
```

## 3. Installing Plume Lite

```bash
# PC
scp -r deploy serveur:/tmp/plume-lite
```

```bash
sudo install -d -m 0755 /srv/plume-lite
sudo install -m 0644 /tmp/plume-lite/compose.yaml /tmp/plume-lite/livekit.yaml /srv/plume-lite/
sudo install -m 0600 /tmp/plume-lite/.env.example /srv/plume-lite/.env
```

Complete `.env` (`sudo nano /srv/plume-lite/.env`): `PUBLIC_IP`, the server's IPv4 address
(`ip -4 addr show ens3`), and `LIVEKIT_SECRET`, the secret shared by Plume Lite and its voice
server, to be generated once:

```bash
openssl rand -hex 32
```

Then:

```bash
cd /srv/plume-lite && sudo docker compose pull && sudo docker compose up --detach --wait
```

`.env.example` already holds the version to run (`PLUME_LITE_VERSION`). Check that the server
answers, on the server itself:

```bash
curl -fsS http://127.0.0.1:3200/health/ready
```

## 4. Caddy

```bash
sudo install -m 0644 /tmp/plume-lite/plume-lite.caddy /etc/caddy/
echo 'import /etc/caddy/plume-lite.caddy' | sudo tee -a /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile && sudo systemctl reload caddy
```

Caddy asks for the certificate of `plume.donrgames.fr` on first access (the DNS of step 2 must
already answer). Then, from the PC, open `https://plume.donrgames.fr`: the page explains to your
friends how to download and install the client.

The copy of the server's Caddyfile kept in the statslol repository
(`deploy/server/etc/caddy/Caddyfile`) does not know this `import` line yet: add it there so that it
stays faithful to the server.

## 5. Inviting someone

Registration is closed: a code is needed, which only you can create.

```bash
cd /srv/plume-lite && sudo docker compose exec server plume-lite-server invite
```

By default, a code serves a single sign-up and expires after 7 days (`invite --uses 5 --days 14`
to change that). Send the person the link `https://plume.donrgames.fr` and the code.

## Going from 0.3 to 0.4 (voice)

0.4 adds a second container, LiveKit, which relays the sound. After publishing the version
(step 1):

```bash
# PC
scp -r deploy serveur:/tmp/plume-lite
```

```bash
sudo install -m 0644 /tmp/plume-lite/compose.yaml /tmp/plume-lite/livekit.yaml /srv/plume-lite/
sudo install -m 0644 /tmp/plume-lite/plume-lite.caddy /etc/caddy/
openssl rand -hex 32
sudo nano /srv/plume-lite/.env
```

In `.env`: `PLUME_LITE_VERSION=0.4.0`, then add the last three lines of `.env.example`:
`LIVEKIT_URL=wss://plume.donrgames.fr`, `PUBLIC_IP=` followed by the server's IPv4 address
(`ip -4 addr show ens3`), and `LIVEKIT_SECRET=` followed by the secret printed by `openssl`.

```bash
cd /srv/plume-lite && sudo docker compose pull && sudo docker compose up --detach --wait
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile && sudo systemctl reload caddy
```

Check that LiveKit answers, then try it with two people in a voice channel:

```bash
curl -fsS http://127.0.0.1:7880/
```

## Administration

All the commands are run in `/srv/plume-lite`, in the form
`sudo docker compose exec server plume-lite-server <command>`:

| Command | Effect |
|---|---|
| `invite [--uses N] [--days N]` | Creates a registration code |
| `invites` | Lists the codes still valid |
| `revoke-invite CODE` | Deletes a code |
| `users` | Lists the accounts (creation, last activity, devices, state) |
| `reset-password NAME` | Gives a new password and logs out all its devices |
| `disable NAME` / `enable NAME` | Disables (and logs out) or re-enables an account |
| `backup` | Backs up the database now |

## Backups

The server backs up the database by itself once a day, in the Docker volume (`/data/backups/`), and
keeps the last 7. They are on the same disk as the database: to protect yourself from losing the
VPS, bring one back to the PC from time to time.

```bash
cd /srv/plume-lite && sudo docker compose exec server ls -l /data/backups
sudo docker compose cp server:/data/backups/<file> /tmp/plume-lite-backup.db && sudo chown ubuntu /tmp/plume-lite-backup.db
```

```bash
# PC
scp serveur:/tmp/plume-lite-backup.db ~/ && ssh serveur rm /tmp/plume-lite-backup.db
```

A backup is a complete database: to restore it, stop the server (`sudo docker compose stop`), copy
it in place of `/data/plume-lite.db` in the volume, then start again.

## Updating

A version that changes the database (0.5.0 rebuilds the messages table) transforms it by itself at
start-up, in a few seconds. To be safe, make a backup just before:
`sudo docker compose exec server plume-lite-server backup`.

After publishing a new version (step 1):

```bash
cd /srv/plume-lite && sudo sed -i 's/^PLUME_LITE_VERSION=.*/PLUME_LITE_VERSION=0.6.3/' .env
sudo docker compose pull && sudo docker compose up --detach --wait
```

The clients are in the image: the download links move to the new version at the same time as the
server, and the clients already installed show "Plume Lite 0.6.3 est disponible" (is available).
During the few seconds of the restart, the connected clients reconnect by themselves. Calls do not
go through this container: LiveKit is only restarted if its image or its configuration changed, and
the server asks it at start-up who is in voice. When in doubt, update when nobody is in a call.
