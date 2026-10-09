# Mettre Plume Lite en ligne

Plume Lite tourne sur le même VPS OVH que statslol et donrgames.fr, dans un conteneur Docker,
derrière le Caddy du serveur, à l'adresse `https://plume.donrgames.fr`. Ce dossier contient tout
ce qui va sur le serveur ; aucune adresse, aucun mot de passe ni aucune clé : le dépôt n'en a pas
besoin.

Les commandes marquées « PC » se lancent sur ton ordinateur, depuis la racine du dépôt ; elles
joignent le serveur par son alias SSH (`Host serveur` dans `~/.ssh/config`). Les autres se lancent
sur le serveur, en tant que `ubuntu` (`sudo` demande son mot de passe).

## Ce qui est déjà en place

Le serveur a été préparé pour statslol (voir `deploy/server/README.md` dans statslol) : SSH par
clé uniquement, pare-feu (seuls 22, 80 et 443 répondent), mises à jour automatiques, Docker, et
Caddy qui obtient tout seul les certificats HTTPS. Docker est déjà connecté au registre privé de
GitHub (`docker login ghcr.io` avec un jeton `read:packages`) : ce jeton donne accès à toutes les
images privées du compte DonRosio, donc aussi à celle de Plume Lite. Plume Lite n'ouvre aucun
nouveau port : tout passe par le 443 de Caddy.

## 1. Publier une version

Chaque version est une étiquette Git `v<version de Cargo.toml>`. Le workflow **Release** compile
le client Windows, le met dans l'image du serveur, essaie l'image comme en production, puis la
publie : `ghcr.io/donrosio/plume-lite-server:<version>`.

```bash
# PC
git tag v0.3.0 && git push origin v0.3.0
```

Attendre que le workflow Release soit vert (onglet Actions de GitHub).

## 2. Le nom de domaine

Dans l'espace client OVH, zone DNS de `donrgames.fr` : ajouter un enregistrement **A**
`plume` vers l'adresse IPv4 du serveur (la même que `statslol`). Comme pour statslol, pas
d'enregistrement AAAA. La propagation prend de quelques minutes à une heure :

```bash
# PC
dig +short plume.donrgames.fr
```

## 3. Installer Plume Lite

```bash
# PC
scp -r deploy serveur:/tmp/plume-lite
```

```bash
sudo install -d -m 0755 /srv/plume-lite
sudo install -m 0644 /tmp/plume-lite/compose.yaml /srv/plume-lite/
sudo install -m 0600 /tmp/plume-lite/.env.example /srv/plume-lite/.env
cd /srv/plume-lite && sudo docker compose pull && sudo docker compose up --detach --wait
```

`.env.example` contient déjà la version à lancer (`PLUME_LITE_VERSION`). Vérifier que le serveur
répond, sur le serveur lui-même :

```bash
curl -fsS http://127.0.0.1:3200/health/ready
```

## 4. Caddy

```bash
sudo install -m 0644 /tmp/plume-lite/plume-lite.caddy /etc/caddy/
echo 'import /etc/caddy/plume-lite.caddy' | sudo tee -a /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile && sudo systemctl reload caddy
```

Caddy demande le certificat de `plume.donrgames.fr` au premier accès (le DNS de l'étape 2 doit
déjà répondre). Puis, depuis le PC, ouvrir `https://plume.donrgames.fr` : la page explique à tes
amis comment télécharger et installer le client.

La copie du Caddyfile du serveur gardée dans le dépôt statslol
(`deploy/server/etc/caddy/Caddyfile`) ne connaît pas encore cette ligne `import` : l'y ajouter
pour qu'elle reste fidèle au serveur.

## 5. Inviter quelqu'un

Les inscriptions sont fermées : il faut un code, que toi seul peux créer.

```bash
cd /srv/plume-lite && sudo docker compose exec server plume-lite-server invite
```

Par défaut, un code sert à une seule inscription et expire au bout de 7 jours
(`invite --uses 5 --days 14` pour en changer). Envoie à la personne : le lien
`https://plume.donrgames.fr` et le code.

## Administration

Toutes les commandes se lancent dans `/srv/plume-lite`, sous la forme
`sudo docker compose exec server plume-lite-server <commande>` :

| Commande | Effet |
|---|---|
| `invite [--uses N] [--days N]` | Crée un code d'inscription |
| `invites` | Liste les codes encore valables |
| `revoke-invite CODE` | Supprime un code |
| `users` | Liste les comptes (création, dernière activité, appareils, état) |
| `reset-password NOM` | Donne un nouveau mot de passe et déconnecte tous ses appareils |
| `disable NOM` / `enable NOM` | Désactive (et déconnecte) ou réactive un compte |
| `backup` | Sauvegarde la base maintenant |

## Sauvegardes

Le serveur sauvegarde la base tout seul une fois par jour, dans le volume Docker
(`/data/backups/`), et garde les 7 dernières. Elles sont sur le même disque que la base : pour
se protéger d'une perte du VPS, en rapatrier une de temps en temps sur le PC.

```bash
cd /srv/plume-lite && sudo docker compose exec server ls -l /data/backups
sudo docker compose cp server:/data/backups/<fichier> /tmp/plume-lite-backup.db && sudo chown ubuntu /tmp/plume-lite-backup.db
```

```bash
# PC
scp serveur:/tmp/plume-lite-backup.db ~/ && ssh serveur rm /tmp/plume-lite-backup.db
```

Une sauvegarde est une base complète : pour la restaurer, arrêter le serveur
(`sudo docker compose stop`), la copier à la place de `/data/plume-lite.db` dans le volume, puis
relancer.

## Mettre à jour

Après avoir publié une nouvelle version (étape 1) :

```bash
cd /srv/plume-lite && sudo sed -i 's/^PLUME_LITE_VERSION=.*/PLUME_LITE_VERSION=0.3.1/' .env
sudo docker compose pull && sudo docker compose up --detach --wait
```

Le client Windows est dans l'image : le lien de téléchargement passe à la nouvelle version en même
temps que le serveur, et les clients déjà installés affichent « Plume Lite 0.3.1 est disponible ».
Pendant les quelques secondes du redémarrage, les clients connectés se reconnectent tout seuls ;
une fois le vocal arrivé, mieux vaudra mettre à jour quand personne n'est en appel.
