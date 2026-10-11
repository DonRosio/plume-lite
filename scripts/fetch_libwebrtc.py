#!/usr/bin/env python3
"""Downloads the prebuilt libwebrtc of the voice engine and checks it against the SHA-256
pinned in scripts/libwebrtc.json (Python 3 standard library only).

    python3 scripts/fetch_libwebrtc.py <linux-x64-release|win-x64-release> <directory>

Prints the directory to give to the build in LK_CUSTOM_WEBRTC: LiveKit's own build script
then uses it instead of downloading an archive it does not check. Fails if Cargo.lock uses
another version of webrtc-sys-build than the pinned one (its libwebrtc may differ), or if the
archive does not match.
"""

import hashlib
import json
import pathlib
import re
import sys
import urllib.request
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
URL = "https://github.com/livekit/rust-sdks/releases/download/{tag}/webrtc-{triple}.zip"


def locked_version(crate: str) -> str:
    lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
    found = re.search(r'\[\[package\]\]\nname = "' + re.escape(crate) + r'"\nversion = "([^"]+)"', lock)
    if not found:
        sys.exit(f"{crate} is not in Cargo.lock")
    return found.group(1)


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    triple, destination = sys.argv[1], pathlib.Path(sys.argv[2]).resolve()
    pins = json.loads((ROOT / "scripts" / "libwebrtc.json").read_text(encoding="utf-8"))
    locked = locked_version("webrtc-sys-build")
    if locked != pins["webrtc-sys-build"]:
        sys.exit(
            f"Cargo.lock uses webrtc-sys-build {locked}, scripts/libwebrtc.json pins "
            f"{pins['webrtc-sys-build']}: pin the libwebrtc of the new version (see the comment there)."
        )
    expected = pins["sha256"].get(triple) or sys.exit(f"no pinned digest for {triple}")

    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / f"webrtc-{triple}.zip"
    url = URL.format(tag=pins["tag"], triple=triple)
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=60) as response, archive.open("wb") as file:
        while chunk := response.read(1 << 20):
            digest.update(chunk)
            file.write(chunk)
    if digest.hexdigest() != expected:
        archive.unlink()
        sys.exit(f"{url}: SHA-256 {digest.hexdigest()}, expected {expected}. Refusing to build with it.")

    with zipfile.ZipFile(archive) as zipped:
        for name in zipped.namelist():
            # The archive only holds `{triple}/…`: nothing may land elsewhere.
            target = (destination / name).resolve()
            if destination != target and destination not in target.parents:
                sys.exit(f"unexpected path in the archive: {name}")
        zipped.extractall(destination)
    archive.unlink()
    print(destination / triple)


if __name__ == "__main__":
    main()
