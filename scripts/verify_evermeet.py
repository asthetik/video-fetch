#!/usr/bin/env python3
"""Check out-of-band that the pinned macOS ffmpeg zip is really evermeet's.

fetch_sidecars.py pins evermeet's zip on first sight, because evermeet
publishes no checksum file, so nothing in the ordinary build path proves
where those bytes came from. This script does, and holds two things true:

  provenance  the detached .sig evermeet publishes next to the zip verifies
              against the key evermeet publishes, over a scratch keyring —
              your own GnuPG keyring is never touched. The key's fingerprint
              is checked before it is trusted, and the signer's chain is
              checked to the same key afterwards.
  integrity   the download hashes to the digest pinned in
              scripts/sidecar_pins.json.

The artifact name comes from FFMPEG_VERSION, the same constant the pins are
generated from, so a version bump carries over without editing this file.

Needs gpg on PATH (`brew install gnupg`) and network access to evermeet.cx.
Downloads into a temp dir that is removed on exit.

    python scripts/verify_evermeet.py

Prints OK and exits 0 when both checks hold; dies on the first failure.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from fetch_sidecars import FFMPEG_VERSION, PINS_FILE, load_pins, sha256_file  # noqa: E402

EVERMEET_BASE = "https://evermeet.cx/ffmpeg"
EVERMEET_KEY_ID = "0x1A660874"
# The primary key behind EVERMEET_KEY_ID, as evermeet publishes it. Releases
# are signed by a signing subkey, so the chain is pinned to the primary.
EVERMEET_FPR = "20F6EA3E0CFD6B4C53447A73476C4B611A660874"

_FPR_RE = re.compile(r"\b[0-9A-F]{40}\b")


def die(msg: str, code: int = 1) -> None:
    print(msg, file=sys.stderr)
    raise SystemExit(code)


def fetch_bytes(url: str) -> bytes:
    print(f"  GET {url}")
    try:
        with urllib.request.urlopen(url) as resp:
            return resp.read()
    except (urllib.error.URLError, OSError) as e:
        die(f"failed to fetch {url}: {e}")


def gpg(*args: str, home: Path | None = None) -> subprocess.CompletedProcess[str]:
    cmd = ["gpg", "--batch", "--no-tty"]
    if home is not None:
        cmd += ["--homedir", str(home)]
    cmd += list(args)
    try:
        return subprocess.run(cmd, capture_output=True, text=True)
    except FileNotFoundError:
        die("gpg not found on PATH (macOS: `brew install gnupg`)")
    except OSError as e:
        die(f"failed to run gpg: {e}")


def fingerprint_from_colons(text: str) -> str:
    """Primary-key fingerprint in `gpg --with-colons` output."""
    for line in text.splitlines():
        if line.startswith("fpr:"):
            fields = line.split(":")
            if len(fields) > 9 and fields[9]:
                return fields[9]
    die("no public key in the exported key block")


def signer_from_status(text: str) -> str:
    """Signing-subkey fingerprint from `gpg --status-fd 1 --verify` output."""
    for line in text.splitlines():
        if not line.startswith("[GNUPG:] VALIDSIG "):
            continue
        fprs = _FPR_RE.findall(line)
        # Subkey first, primary last; asserting on the primary covers both.
        if EVERMEET_FPR not in fprs:
            who = fprs[0] if fprs else "an unknown key"
            die(f"signature is from {who}, not evermeet's {EVERMEET_FPR}")
        return fprs[0]
    die("gpg reported no VALIDSIG; the signature did not verify")


def main() -> None:
    pins = load_pins()
    artifact = f"ffmpeg-{FFMPEG_VERSION}.zip"
    expected = pins["downloads"].get(artifact)
    if expected is None:
        die(
            f"{artifact} is not pinned in {PINS_FILE.name}; refresh it with\n"
            "  python scripts/fetch_sidecars.py --update-pins"
        )

    with tempfile.TemporaryDirectory(prefix="evermeet-verify-") as tmp_s:
        work = Path(tmp_s)
        home = work / "gnupg"
        home.mkdir(mode=0o700)

        print(f"downloading {artifact} from evermeet.cx ...")
        zip_path = work / artifact
        zip_path.write_bytes(fetch_bytes(f"{EVERMEET_BASE}/{artifact}"))
        sig_path = work / f"{artifact}.sig"
        sig_path.write_bytes(fetch_bytes(f"{EVERMEET_BASE}/{artifact}.sig"))
        key_path = work / "evermeet.asc"
        key_path.write_bytes(fetch_bytes(f"{EVERMEET_BASE}/{EVERMEET_KEY_ID}.asc"))

        shown = gpg(
            "--with-colons", "--import-options", "show-only", "--import", str(key_path)
        )
        if shown.returncode != 0:
            die(f"gpg could not read {EVERMEET_KEY_ID}.asc:\n{shown.stderr.strip()}")
        fpr = fingerprint_from_colons(shown.stdout)
        if fpr != EVERMEET_FPR:
            die(
                f"the key at {EVERMEET_BASE}/{EVERMEET_KEY_ID}.asc has fingerprint\n"
                f"  {fpr}\nbut evermeet's key is\n  {EVERMEET_FPR}"
            )
        print("key fingerprint matches evermeet's published key")

        imported = gpg("--import", str(key_path), home=home)
        if imported.returncode != 0:
            die(f"gpg could not import the key into a scratch keyring:\n"
                f"{imported.stderr.strip()}")

        verified = gpg(
            "--status-fd", "1", "--verify", str(sig_path), str(zip_path), home=home
        )
        if verified.returncode != 0:
            die(f"gpg rejected the signature on {artifact}:\n{verified.stderr.strip()}")
        print(f"signed by {signer_from_status(verified.stdout)} (subkey of {EVERMEET_FPR})")

        actual = sha256_file(zip_path)
        if actual != expected:
            die(
                f"SHA-256 mismatch for {artifact}:\n"
                f"  pinned  {expected}\n"
                f"  served  {actual}\n"
                "The signature is good, so evermeet.cx is serving bytes that "
                "differ from the pin. Treat that as a supply-chain incident "
                "rather than as a pin to refresh."
            )
        print(f"digest matches scripts/{PINS_FILE.name}")

    print("OK")


if __name__ == "__main__":
    main()
