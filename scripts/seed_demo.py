#!/usr/bin/env python3
"""Fills a local Plume Lite server with demo data (Python 3 standard library only).

    python3 scripts/seed_demo.py [http://127.0.0.1:8080]

Creates (or reuses) the accounts alice, bob and carol — password: motdepasse-demo —
a "Rust France" server with three channels and a short conversation. Re-running it
does not duplicate anything.
"""

import json
import sys
import time
import urllib.error
import urllib.request

SERVER = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080").rstrip("/")
PASSWORD = "motdepasse-demo"


def call(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(f"{SERVER}/api/v1{path}", data=data, method=method)
    request.add_header("Content-Type", "application/json")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def account(username):
    credentials = {"username": username, "password": PASSWORD}
    status, body = call("POST", "/auth/register", body=credentials)
    if status == 403:
        sys.exit("Inscriptions fermées : ajoutez OPEN_REGISTRATION=true dans .env (cp .env.example .env), "
                 "puis redémarrez le serveur.")
    if status == 409:
        status, body = call("POST", "/auth/login", body=credentials)
    if status not in (200, 201):
        sys.exit(f"{username}: {status} {body}")
    return body["token"]


def main():
    alice, bob, carol = (account(name) for name in ("alice", "bob", "carol"))

    _, guilds = call("GET", "/users/@me/guilds", alice)
    if any(g["name"] == "Rust France" for g in guilds):
        print("Données de démo déjà présentes.")
        return

    _, guild = call("POST", "/guilds", alice, {"name": "Rust France"})
    general = guild["channels"][0]["id"]
    for name in ("annonces", "entraide"):
        call("POST", f"/guilds/{guild['id']}/channels", alice, {"name": name})
    _, invite = call("POST", f"/guilds/{guild['id']}/invites", alice, {})
    for token in (bob, carol):
        call("POST", f"/invites/{invite['code']}", token)

    conversation = [
        (alice, "Salut tout le monde 👋 Bienvenue sur le serveur Rust France !"),
        (bob, "Merci ! Ça tourne vraiment en natif ce client ?"),
        (alice, "Oui : Rust + iced, pas d'Electron. Une seule fenêtre, un seul processus."),
        (carol, "Et le temps réel passe par un WebSocket, c'est instantané."),
        (bob, "Vous avez prévu les salons vocaux ?"),
        (alice, "Pas dans le MVP : c'est sur la roadmap (WebRTC via un SFU)."),
    ]
    for token, content in conversation:
        status, body = call("POST", f"/channels/{general}/messages", token, {"content": content})
        if status != 201:
            sys.exit(f"message: {status} {body}")
        time.sleep(0.3)  # stays under the 5 messages / 5 s rate limit

    print(f"OK : connectez-vous avec alice, bob ou carol (mot de passe : {PASSWORD}). Invitation : {invite['code']}")


if __name__ == "__main__":
    main()
