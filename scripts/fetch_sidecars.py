#!/usr/bin/env python3
"""Download yt-dlp + ffmpeg into src-tauri/binaries/ for Tauri externalBin.

Versions are pinned, not floating:
  yt-dlp  — scripts/requirements-sidecars.txt (`yt-dlp==<PyPI version>`,
            auto-upgraded by Dependabot; zero-padded back to the GitHub
            release tag here)
  ffmpeg  — FFMPEG_VERSION + FFMPEG_BTBN_TAG below: a specific BtbN
            autobuild snapshot for Linux/Windows, the martin-riedl arm64
            build FFMPEG_MACOS_BUILD for macOS; no package registry exists
            for Dependabot to track, so bump it manually

Integrity:
  Every downloaded artifact is checked against a SHA-256 digest pinned in
  scripts/sidecar_pins.json BEFORE it is extracted. The BtbN asset names
  and their digests are stored per snapshot, so the mutable `latest` tag
  is never used and an upstream rebuild fails the build instead of
  silently shipping different bytes.

  To bump yt-dlp: let Dependabot edit requirements-sidecars.txt, then add
  the new artifacts' digests:
      python scripts/fetch_sidecars.py --update-pins
  To bump ffmpeg: set FFMPEG_VERSION, FFMPEG_BTBN_TAG and
  FFMPEG_MACOS_BUILD, then run the same --update-pins. New assets are
  appended; existing digests are never overwritten unless --force is
  passed, so a digest failure cannot be waved away by re-running the
  update (a human must look at the diff). yt-dlp, BtbN and martin-riedl
  all publish checksum files that --update-pins cross-checks.

  Pin keys carry what identifies the upstream release: BtbN's and
  martin-riedl's asset names embed the build, and yt-dlp's
  version-independent names get the release tag appended
  (`yt-dlp_linux@2026.08.19`). A bump therefore adds keys, which
  --update-pins records without --force while pruning the superseded
  build's keys; a digest that changes under an unchanged key stays an
  incident.

  The pins file also records the ffmpeg snapshot it was generated from
  (FFMPEG_VERSION / FFMPEG_BTBN_TAG / FFMPEG_MACOS_BUILD), and every fetch
  compares the recorded values against the constants before touching a
  byte, dying with the --update-pins instruction on a mismatch. That is
  what makes a forgotten ffmpeg bump fail loudly instead of silently
  fetching the superseded build: the asset names and digests come from the
  file itself, so no re-fetch can pick a bump up — regenerating the pins
  is the only fix.

  The macOS binary is additionally asserted to be an arm64 Mach-O and,
  on macOS, a Developer ID-signed binary: evermeet's Intel-only build was
  shipped as the macOS sidecar in 0.4.0 and could not run on Apple
  silicon, silently leaving every video unmerged. Run
  scripts/verify_macos_ffmpeg.py out-of-band to re-confirm the pinned zip
  against upstream's checksum, architecture and signature.

  Every platform's ffmpeg carries the CPU its asset name claims: macOS
  through the Mach-O header above, Linux and Windows through the ELF
  e_machine / PE Machine field. The Linux/Windows assertion keys off the
  same BtbN arch token the asset name is built from, so a platform added
  to BTBN_ARCHES without an assertion entry fails the build instead of
  shipping unchecked.

Naming:
  binaries/yt-dlp-{TARGET_TRIPLE}[.exe]
  binaries/ffmpeg-{TARGET_TRIPLE}[.exe]
TARGET_TRIPLE from: rustc --print host-tuple

Run before `npm run tauri build` (CI release) or local bundling.
"""

from __future__ import annotations

import hashlib
import json
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from collections.abc import Callable
from pathlib import Path

YTDLP_DOWNLOAD_BASE = "https://github.com/yt-dlp/yt-dlp/releases/download"
FFMPEG_BTBN_TAG = "autobuild-2026-09-20-13-11"
FFMPEG_BTBN_BASE = (
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/" + FFMPEG_BTBN_TAG
)
FFMPEG_MACOS_BASE = "https://ffmpeg.martin-riedl.de/download/macos/arm64"
# Immutable build id (timestamp_version) in the path. Never the
# /redirect/latest/... endpoint: it floats to whatever upstream published
# last, which would silently invalidate the pinned digest.
FFMPEG_MACOS_BUILD = "1789931890_9.0.2"
FFMPEG_MACOS_SIGNER = "Martin Riedl"
FFMPEG_MACOS_TEAM_ID = "KU3N25YGLU"

# martin-riedl.de answers Python-urllib's default User-Agent with 403.
HTTP_USER_AGENT = "videofetch-fetch-sidecars/1.0"

# FFmpeg release branch to ship (x.y.z). Linux/Windows use the pinned BtbN
# snapshot's matching release-branch builds (n<x.y>); macOS uses the
# martin-riedl arm64 build pinned as FFMPEG_MACOS_BUILD. Bumped manually —
# see the module docstring.
FFMPEG_VERSION = "9.0.2"

REQUIREMENTS_FILE = Path(__file__).resolve().parent / "requirements-sidecars.txt"
PINS_FILE = Path(__file__).resolve().parent / "sidecar_pins.json"
# Tauri's externalBin directory, where the fetched sidecars land.
SIDECAR_DIR = Path(__file__).resolve().parent.parent / "src-tauri" / "binaries"

# Schema 2 records the ffmpeg snapshot the file was generated from
# (`ffmpeg_snapshot`). Schema 1 files still load so --update-pins can
# regenerate them; the fetch path refuses them until then.
PINS_SCHEMA = 2

# The snapshot anchor's members: the constants the pins were generated from.
_FFMPEG_SNAPSHOT_KEYS = ("version", "btbn_tag", "macos_build")

USAGE = """usage: fetch_sidecars.py [--update-pins [--force]]

  (no flags)     download and integrity-check sidecars for this host
  --update-pins  refresh missing or changed digests in sidecar_pins.json
                 (downloads every platform's artifacts; never overwrites
                 an existing digest without --force)
"""


def die(msg: str, code: int = 1) -> None:
    print(msg, file=sys.stderr)
    raise SystemExit(code)


def die_unverified(dest: Path, msg: str) -> None:
    """Delete the failed artifact at `dest`, then die with `msg`.

    fetch_sidecars re-checks digests on its next run, but a `tauri build` in
    between bundles whatever sits at the destination — for yt-dlp that is the
    final sidecar path — so bytes that failed their checksum, were served
    without one, or arrived only partially must not survive the failure.
    """
    dest.unlink(missing_ok=True)
    die(msg)


def verify_written(path: Path, verify: Callable[..., None], *args: object) -> None:
    """Run `verify(path, *args)`; if it dies, remove the bytes it rejected.

    fetch_ffmpeg copies the extracted binary to its final sidecar path and
    only then checks arch or signature. A die() from that check must not
    leave the rejected bytes there: the next `tauri build` would bundle
    them. The verifiers stay pure — this wrapper owns the cleanup, the
    same way download_and_verify does on the download paths.
    """
    try:
        verify(path, *args)
    except SystemExit:
        path.unlink(missing_ok=True)
        raise


def host_triple() -> str:
    try:
        out = subprocess.check_output(
            ["rustc", "--print", "host-tuple"],
            text=True,
        ).strip()
    except (OSError, subprocess.CalledProcessError) as e:
        die(f"failed to run rustc --print host-tuple: {e}")
    if not out:
        die("rustc --print host-tuple returned empty output")
    return out


def detect_system() -> str:
    return platform.system()


def detect_machine() -> str:
    return platform.machine().lower()


_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_sha256(what: str, expected: str, actual: str) -> None:
    if actual.lower() != expected.lower():
        die(
            f"SHA-256 mismatch for {what}:\n"
            f"  expected {expected}\n"
            f"  actual   {actual}\n"
            "Refusing to use this artifact. If upstream legitimately "
            "rebuilt it, inspect the change and refresh the pin with "
            "`python scripts/fetch_sidecars.py --update-pins --force`."
        )


_MH_MAGIC_64 = 0xFEEDFACF
_FAT_MAGIC = 0xCAFEBABE
_FAT_MAGIC_64 = 0xCAFEBABF
_CPU_TYPE_ARM64 = 0x0100000C
_EM_X86_64 = 0x3E
_EM_AARCH64 = 0xB7
_IMAGE_FILE_MACHINE_AMD64 = 0x8664
_IMAGE_FILE_MACHINE_ARM64 = 0xAA64


def macho_cpu_types(data: bytes) -> set[int]:
    """CPU-type constants from Mach-O header bytes (thin 64-bit or universal).

    Raises ValueError when the bytes are not a 64-bit Mach-O file. Fat
    headers are big-endian; a thin header is accepted in the little-endian
    MH_MAGIC_64 form Apple toolchains emit.
    """
    if len(data) < 8:
        raise ValueError("too small to hold a Mach-O header")
    magic = int.from_bytes(data[0:4], "big")
    if magic in (_FAT_MAGIC, _FAT_MAGIC_64):
        count = int.from_bytes(data[4:8], "big")
        stride = 20 if magic == _FAT_MAGIC else 32
        types: set[int] = set()
        for index in range(count):
            offset = 8 + index * stride
            if offset + 4 > len(data):
                raise ValueError("fat header truncated")
            types.add(int.from_bytes(data[offset : offset + 4], "big"))
        return types
    if int.from_bytes(data[0:4], "little") == _MH_MAGIC_64:
        return {int.from_bytes(data[4:8], "little")}
    raise ValueError("not a 64-bit Mach-O file")


def elf_machine(data: bytes) -> int:
    """e_machine from a 64-bit ELF header.

    Raises ValueError when the bytes are not a 64-bit ELF file.
    """
    if len(data) < 20:
        raise ValueError("too small to hold an ELF header")
    if data[:4] != b"\x7fELF":
        raise ValueError("not an ELF file")
    if data[4] != 2:
        raise ValueError("not a 64-bit ELF file")
    order = "little" if data[5] == 1 else "big"
    return int.from_bytes(data[18:20], order)


def pe_machine(data: bytes) -> int:
    """COFF Machine field from a PE header.

    Raises ValueError when the bytes are not a PE file. The DOS stub is
    skipped through e_lfanew at 0x3C, the only fixed offset before the
    signature.
    """
    if len(data) < 0x40:
        raise ValueError("too small to hold a DOS header")
    if data[:2] != b"MZ":
        raise ValueError("not a PE file")
    offset = int.from_bytes(data[0x3C:0x40], "little")
    if len(data) < offset + 6:
        raise ValueError("PE header offset points past the end of the file")
    if data[offset : offset + 4] != b"PE\0\0":
        raise ValueError("not a PE file")
    return int.from_bytes(data[offset + 4 : offset + 6], "little")


def verify_macos_ffmpeg(path: Path) -> None:
    """Assert the macOS sidecar is an arm64 binary signed by our builder.

    The header check runs on every host; codesign needs macOS tooling; the
    execute probe needs an arm64 host to mean anything (GitHub's arm64
    runners carry Rosetta, so running an Intel binary there proves
    nothing — the Mach-O header is the check).
    """
    with path.open("rb") as f:
        header = f.read(4096)
    try:
        cpu_types = macho_cpu_types(header)
    except ValueError as e:
        die(f"{path} is not a usable macOS ffmpeg binary: {e}")
    if _CPU_TYPE_ARM64 not in cpu_types:
        die(
            f"{path} carries CPU types {sorted(hex(t) for t in cpu_types)}, "
            "not arm64.\nThis is the 0.4.0 failure mode (an Intel-only "
            "binary shipped as the macOS sidecar): it cannot run on Apple "
            "silicon, so yt-dlp cannot merge and downloads come out as "
            "silent fragments. Refusing to bundle it."
        )
    print("  ok       Mach-O arm64")

    if detect_system() != "Darwin":
        return
    codesign = shutil.which("codesign")
    if codesign is None:
        die("codesign not found; cannot verify the macOS ffmpeg signature")
    verified = subprocess.run(
        [codesign, "--verify", "--strict", str(path)],
        capture_output=True,
        text=True,
    )
    if verified.returncode != 0:
        die(f"codesign --verify failed for {path}:\n{verified.stderr.strip()}")
    details = subprocess.run(
        [codesign, "-dvvv", str(path)], capture_output=True, text=True
    )
    # `--verify --strict` above proves the seal; the Authority lines prove
    # *whose* seal: a leaf certificate issued to our builder, chained up to
    # Apple. Matching TeamIdentifier alone would also pass for any signature
    # that merely carries that string.
    chain = [
        f"Authority=Developer ID Application: {FFMPEG_MACOS_SIGNER} "
        f"({FFMPEG_MACOS_TEAM_ID})",
        "Authority=Developer ID Certification Authority",
        "Authority=Apple Root CA",
    ]
    signature = details.stdout + details.stderr
    missing = [line for line in chain if line not in signature]
    if missing:
        die(
            f"{path} does not carry a Developer ID signature chain from "
            f"{FFMPEG_MACOS_SIGNER} (team {FFMPEG_MACOS_TEAM_ID}) up to "
            f"Apple; missing {missing}; refusing to bundle it"
        )
    print(
        f"  ok       Developer ID signature "
        f"({FFMPEG_MACOS_SIGNER}, team {FFMPEG_MACOS_TEAM_ID})"
    )

    if detect_machine() in {"arm64", "aarch64"}:
        try:
            probe = subprocess.run(
                [str(path), "-version"], capture_output=True, text=True
            )
        except OSError as e:
            die(f"{path} could not be executed: {e}")
        if probe.returncode != 0:
            die(f"{path} failed to run: {probe.stderr.strip()}")
        first_line = probe.stdout.splitlines()[0] if probe.stdout else ""
        print(f"  ok       runs ({first_line})")


def load_pins(path: Path | None = None) -> dict:
    """Return {'downloads': {artifact: sha256}, 'binaries': {kind: {triple: sha256}}}."""
    file = path or PINS_FILE
    try:
        data = json.loads(file.read_text(encoding="utf-8"))
    except OSError as e:
        die(
            f"cannot read sidecar pin file {file}: {e}\n"
            "it must be committed; run `python scripts/fetch_sidecars.py "
            "--update-pins` to regenerate it"
        )
    except json.JSONDecodeError as e:
        die(f"{file} is not valid JSON: {e}")
    if not isinstance(data, dict):
        die(f"{file} must contain a JSON object")
    schema = data.get("schema", 1)
    if schema not in (1, PINS_SCHEMA):
        die(
            f"{file}: unsupported schema version {schema!r}; this script "
            f"writes schema {PINS_SCHEMA}"
        )
    snapshot = data.get("ffmpeg_snapshot")
    if snapshot is None:
        if schema >= PINS_SCHEMA:
            die(
                f"{file}: schema {PINS_SCHEMA} must record the ffmpeg "
                "snapshot it was generated from; regenerate it with:\n"
                "  python scripts/fetch_sidecars.py --update-pins"
            )
    elif (
        not isinstance(snapshot, dict)
        or set(snapshot) != set(_FFMPEG_SNAPSHOT_KEYS)
        or not all(
            isinstance(snapshot[key], str) and snapshot[key]
            for key in _FFMPEG_SNAPSHOT_KEYS
        )
    ):
        die(
            f"{file}: 'ffmpeg_snapshot' must be an object with the string "
            f"members {sorted(_FFMPEG_SNAPSHOT_KEYS)}"
        )
    downloads = data.get("downloads")
    if not isinstance(downloads, dict):
        die(f"{file}: 'downloads' must be an object of digest strings")
    for name, digest in downloads.items():
        if not isinstance(digest, str) or not _SHA256_RE.match(digest.lower()):
            die(f"{file}: downloads.{name!r} is not a SHA-256 digest")
    binaries = data.setdefault("binaries", {})
    if not isinstance(binaries, dict):
        die(f"{file}: 'binaries' must be an object keyed by sidecar name")
    for kind, triples in binaries.items():
        if not isinstance(triples, dict):
            die(f"{file}: binaries.{kind!r} must be an object keyed by target triple")
        for triple, digest in triples.items():
            if not isinstance(digest, str) or not _SHA256_RE.match(digest.lower()):
                die(f"{file}: binaries.{kind}.{triple!r} is not a SHA-256 digest")
    return data


def write_pins(pins: dict, path: Path | None = None) -> None:
    file = path or PINS_FILE
    payload = {
        "schema": PINS_SCHEMA,
        "//": (
            "SHA-256 pins for artifacts downloaded by fetch_sidecars.py. "
            "Update with --update-pins; see the script docstring."
        ),
        # Stamped from the constants, not from `pins`: the digests written
        # here were resolved from them, and it is what lets --update-pins
        # migrate a schema-1 file (which load_pins still accepts) in one run.
        "ffmpeg_snapshot": ffmpeg_snapshot(),
        "downloads": dict(sorted(pins["downloads"].items())),
    }
    if pins.get("binaries"):
        payload["binaries"] = dict(sorted(pins["binaries"].items()))
    file.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {file}")


def ffmpeg_snapshot() -> dict:
    """The pinned ffmpeg snapshot's identity, as recorded in the pins file."""
    return {
        "version": FFMPEG_VERSION,
        "btbn_tag": FFMPEG_BTBN_TAG,
        "macos_build": FFMPEG_MACOS_BUILD,
    }


def _describe_ffmpeg_snapshot(snapshot: dict | None) -> str:
    if snapshot is None:
        return "(none recorded)"
    return (
        f"version {snapshot['version']}, BtbN {snapshot['btbn_tag']}, "
        f"macOS build {snapshot['macos_build']}"
    )


def verify_ffmpeg_snapshot(pins: dict) -> None:
    """Fail loudly when the pins file was not regenerated for the pinned
    ffmpeg snapshot.

    FFMPEG_VERSION / FFMPEG_BTBN_TAG / FFMPEG_MACOS_BUILD are constants in
    this script, but the pinned asset names and their digests are read from
    the pins file: after a bump the file still names the superseded build,
    so a forgotten --update-pins would leave every environment — clean CI
    runners included — silently fetching it. The recorded identity is what
    turns that into a failure, the way the versioned yt-dlp pin keys do
    for a requirements bump. It is deliberately not a re-fetch trigger:
    only regenerating the file can move off the old snapshot.
    """
    recorded = pins.get("ffmpeg_snapshot")
    current = ffmpeg_snapshot()
    if recorded == current:
        return
    if recorded is None:
        what = (
            f"{PINS_FILE} does not record which ffmpeg snapshot it was "
            "generated from"
        )
    else:
        what = f"{PINS_FILE} was generated for another ffmpeg snapshot"
    die(
        f"{what}:\n"
        f"  recorded  {_describe_ffmpeg_snapshot(recorded)}\n"
        f"  current   {_describe_ffmpeg_snapshot(current)}\n"
        "The pinned asset names and digests are taken from the file itself, "
        "so re-fetching cannot pick up a snapshot bump; regenerate the "
        "pins:\n"
        "  python scripts/fetch_sidecars.py --update-pins"
    )


def pinned_digest(pins: dict, artifact: str) -> str:
    digest = pins["downloads"].get(artifact)
    if digest is None:
        die(
            f"no SHA-256 pin for {artifact!r} in {PINS_FILE}\n"
            "if the pinned version was just bumped, record it with:\n"
            "  python scripts/fetch_sidecars.py --update-pins"
        )
    return digest


def pin_key_from_url(url: str) -> str:
    """Artifact basename, the key used in the pins file."""
    name = urllib.parse.urlsplit(url).path.rsplit("/", 1)[-1]
    if not name:
        raise ValueError(f"URL has no artifact basename: {url!r}")
    return name


def parse_shasums(text: str) -> dict[str, str]:
    """Parse sha256sum(1)-format lines (yt-dlp SHA2-256SUMS, BtbN checksums)."""
    entries: dict[str, str] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(maxsplit=1)
        if len(parts) != 2:
            continue
        digest, name = parts[0].lower(), parts[1].strip().lstrip("*")
        if _SHA256_RE.match(digest) and name:
            entries[name] = digest
    return entries


def request_headers(extra: dict[str, str] | None = None) -> dict[str, str]:
    """Headers every request carries: our own User-Agent (martin-riedl 403s
    urllib's default) plus anything the caller adds, such as an API token."""
    headers = {"User-Agent": HTTP_USER_AGENT}
    if extra:
        headers.update(extra)
    return headers


def _open_url(url: str, headers: dict[str, str] | None = None):
    request = urllib.request.Request(url, headers=request_headers(headers))
    return urllib.request.urlopen(request)


def fetch_bytes(url: str, headers: dict[str, str] | None = None) -> bytes:
    print(f"  GET {url}")
    try:
        with _open_url(url, headers) as resp:
            return resp.read()
    except (urllib.error.URLError, OSError) as e:
        die(f"failed to fetch {url}: {e}")


def ytdlp_upstream_shasums(version: str) -> dict[str, str]:
    """Digests yt-dlp publishes for the pinned release tag."""
    url = f"{YTDLP_DOWNLOAD_BASE}/{version}/SHA2-256SUMS"
    return parse_shasums(fetch_bytes(url).decode("utf-8"))


def btbn_upstream_shasums() -> dict[str, str]:
    """Digests BtbN publishes for the pinned snapshot."""
    url = f"{FFMPEG_BTBN_BASE}/checksums.sha256"
    return parse_shasums(fetch_bytes(url).decode("utf-8"))


def download(url: str, dest: Path) -> None:
    print(f"  GET {url}")
    try:
        with _open_url(url) as resp, dest.open("wb") as f:
            shutil.copyfileobj(resp, f)
    except (urllib.error.URLError, OSError) as e:
        die_unverified(dest, f"failed to download {url}: {e}")


def download_and_verify(
    url: str, dest: Path, pins: dict, artifact: str | None = None
) -> None:
    """Download the artifact and record or check it against its digest.

    In update mode (`PinsRecorder`) the digest was recorded from the pinned
    release's published checksums a moment earlier; the download is compared
    against it before the entry is trusted. `artifact` overrides the pins
    key when the URL basename cannot serve as one: the macOS zip is
    `ffmpeg.zip` on every build, and yt-dlp's names carry no version.
    """
    if isinstance(pins, PinsRecorder):
        pins.download_and_record(url, dest, artifact)
        return
    key = artifact or pin_key_from_url(url)
    expected = pinned_digest(pins, key)
    download(url, dest)
    try:
        verify_sha256(key, expected, sha256_file(dest))
    except SystemExit:
        # verify_sha256 reported why; only the cleanup belongs here.
        dest.unlink(missing_ok=True)
        raise


def make_executable(path: Path) -> None:
    if detect_system() == "Windows":
        return
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def normalize_ytdlp_version(raw: str, source: str = "requirements-sidecars.txt") -> str:
    """Convert a PyPI-normalized yt-dlp version to the GitHub release tag.

    PyPI strips leading zeros (2026.08.19 -> 2026.8.19); GitHub release
    tags zero-pad month and day, so pad them back.
    """
    parts = raw.split(".")
    valid = (
        len(parts) == 3
        and all(p.isascii() and p.isdigit() for p in parts)
        and len(parts[0]) == 4
        and 1 <= int(parts[1]) <= 12
        and 1 <= int(parts[2]) <= 31
    )
    if not valid:
        die(
            f"unsupported yt-dlp version {raw!r} in {source}; "
            "expected a stable release like 2026.8.19"
        )
    year, month, day = parts
    return f"{year}.{int(month):02d}.{int(day):02d}"


def load_ytdlp_version(path: Path | None = None) -> str:
    """Return the pinned yt-dlp GitHub tag from requirements-sidecars.txt."""
    file = path or REQUIREMENTS_FILE
    try:
        text = file.read_text(encoding="utf-8")
    except OSError as e:
        die(f"cannot read sidecar version pin file {file}: {e}")
    pin: str | None = None
    for line in text.splitlines():
        m = re.match(r"^\s*yt-dlp\s*==\s*([\w.]+)(?:\s+#.*)?\s*$", line)
        if m:
            if pin is not None:
                die(
                    f"multiple yt-dlp pins found in {file}: "
                    f"{pin!r} and {m.group(1)!r}"
                )
            pin = m.group(1)
    if pin is None:
        die(f"no 'yt-dlp==<version>' pin found in {file}")
    return normalize_ytdlp_version(pin, str(file))


def ytdlp_asset_name(system: str, machine: str) -> str:
    """yt-dlp release asset for a platform (names are version-independent; the
    pin key appends the release tag — see `ytdlp_pin_key`)."""
    m = machine.lower()
    if system == "Darwin":
        return "yt-dlp_macos"
    if system == "Linux":
        if m in {"x86_64", "amd64"}:
            return "yt-dlp_linux"
        if m in {"aarch64", "arm64"}:
            return "yt-dlp_linux_aarch64"
        raise ValueError(f"Unsupported Linux arch for yt-dlp: {machine}")
    if system == "Windows":
        if m in {"aarch64", "arm64"}:
            return "yt-dlp_arm64.exe"
        if m in {"x86_64", "amd64", "x64"}:
            return "yt-dlp.exe"
        raise ValueError(f"Unsupported Windows arch for yt-dlp: {machine}")
    raise ValueError(f"Unsupported OS for yt-dlp: {system}")


def ytdlp_download_url(system: str, machine: str, version: str) -> str:
    return f"{YTDLP_DOWNLOAD_BASE}/{version}/{ytdlp_asset_name(system, machine)}"


def ytdlp_pin_key(artifact: str, version: str) -> str:
    """The pins key for a yt-dlp asset: the version-independent asset name plus
    the release tag (`yt-dlp_linux@2026.08.19`).

    Embedding the version is what lets a bump add a key instead of rewriting
    one, which is how --update-pins records it without --force; a digest that
    changes under an unchanged key stays an incident.
    """
    return f"{artifact}@{version}"


def ffmpeg_branch(version: str = FFMPEG_VERSION) -> str:
    """Release branch of an x.y.z version (9.0.1 -> 9.0), as used by BtbN."""
    parts = version.split(".")
    if len(parts) != 3 or any(not p.isdigit() for p in parts):
        die(f"FFMPEG_VERSION {version!r} must be a full x.y.z release like 9.0.1")
    return ".".join(parts[:2])


BTBN_ARCHES = {
    "Linux": {"x86_64": "linux64", "amd64": "linux64", "aarch64": "linuxarm64", "arm64": "linuxarm64"},
    "Windows": {"x86_64": "win64", "amd64": "win64", "x64": "win64", "aarch64": "winarm64", "arm64": "winarm64"},
}


def btbn_arch_token(system: str, machine: str) -> str:
    """The BtbN asset arch token (linux64, winarm64, ...) for a platform."""
    try:
        return BTBN_ARCHES[system][machine.lower()]
    except KeyError:
        raise ValueError(f"Unsupported OS/arch for ffmpeg: {system}/{machine}")


def btbn_asset_name(system: str, machine: str, downloads: dict | None = None) -> str:
    """The pinned BtbN asset name for a platform.

    The snapshot is pinned in sidecar_pins.json by asset name (BtbN embeds
    the exact build, e.g. ffmpeg-n9.0.2-3-ga5923073bf-..., in the file name,
    which is why the frozen autobuild tag alone is not enough).

    `downloads` overrides the on-disk table, which --update-pins needs: the
    file still holds the superseded build's names until the run writes it,
    so resolving against the run's own (pruned, freshly recorded) table is
    what makes a build bump converge in one pass.
    """
    branch = ffmpeg_branch()
    arch = btbn_arch_token(system, machine)
    ext = "zip" if system == "Windows" else "tar.xz"
    names = [
        name
        for name in (load_pins()["downloads"] if downloads is None else downloads)
        if name.startswith("ffmpeg-n")
        and name.endswith(f"-{arch}-gpl-{branch}.{ext}")
        and "-shared-" not in name
    ]
    if len(names) != 1:
        die(
            f"expected exactly one pinned BtbN asset for {system}/{machine} "
            f"(branch {branch}), found {len(names)}: {sorted(names)}\n"
            "if FFMPEG_BTBN_TAG/FFMPEG_VERSION was just bumped, refresh "
            "with: python scripts/fetch_sidecars.py --update-pins"
        )
    return names[0]


# BtbN asset arch token -> (header parser, machine field, display name).
# btbn_asset_name picks the asset by token, so the assertion keys off the
# same token: a token with no entry here is a hard error, which keeps a
# newly added platform from silently shipping without the check.
_BTBN_ARCH_MACHINES = {
    "linux64": (elf_machine, _EM_X86_64, "x86-64"),
    "linuxarm64": (elf_machine, _EM_AARCH64, "AArch64"),
    "win64": (pe_machine, _IMAGE_FILE_MACHINE_AMD64, "x86-64"),
    "winarm64": (pe_machine, _IMAGE_FILE_MACHINE_ARM64, "ARM64"),
}


def verify_btbn_ffmpeg_arch(path: Path, system: str, machine: str) -> None:
    """Assert a Linux/Windows sidecar carries the CPU its asset name claims.

    macOS gets the same guarantee from the Mach-O check in
    verify_macos_ffmpeg. Without this the only thing tying an asset to a CPU
    is its name, and a mislabelled one would be pinned by its own bytes and
    shipped inside an installer for the wrong architecture.
    """
    token = btbn_arch_token(system, machine)
    entry = _BTBN_ARCH_MACHINES.get(token)
    if entry is None:
        die(f"no arch assertion covers the BtbN asset class {token!r}")
    parse, expected, name = entry
    with path.open("rb") as f:
        header = f.read(4096)
    try:
        actual = parse(header)
    except ValueError as e:
        die(f"{path} is not a usable ffmpeg binary for {system}: {e}")
    if actual != expected:
        die(
            f"{path} carries machine {actual:#06x}, not {expected:#06x} "
            f"({name}) for {system}/{machine}.\nThis is the 0.4.0 failure "
            "mode on another platform: an ffmpeg built for a different CPU "
            "cannot run, so yt-dlp cannot merge and downloads come out as "
            "silent fragments. Refusing to bundle it."
        )
    print(f"  ok       {name} ({token})")


def ffmpeg_download_url(
    system: str, machine: str, downloads: dict | None = None
) -> str | None:
    """Return archive URL, or None for Darwin (macOS handled separately).

    `downloads` is passed through to `btbn_asset_name`.
    """
    if system == "Darwin":
        return None
    return f"{FFMPEG_BTBN_BASE}/{btbn_asset_name(system, machine, downloads)}"


def ffmpeg_macos_url() -> str:
    """Immutable download URL of the pinned macOS arm64 build."""
    return f"{FFMPEG_MACOS_BASE}/{FFMPEG_MACOS_BUILD}/ffmpeg.zip"


def ffmpeg_macos_artifact() -> str:
    """Pins key for the macOS zip: its URL basename (`ffmpeg.zip`) collides
    across builds, so the key carries platform and build id."""
    return f"ffmpeg-macos-arm64-{FFMPEG_MACOS_BUILD}.zip"


def fetch_ytdlp(
    system: str,
    machine: str,
    out: Path,
    version: str,
    pins: dict,
) -> None:
    print(f"Downloading yt-dlp {version}...")
    try:
        artifact = ytdlp_asset_name(system, machine)
        url = ytdlp_download_url(system, machine, version)
    except ValueError as e:
        die(str(e))
    # In update mode `pins` is the PinsRecorder, and download_and_verify's
    # isinstance dispatch checks the downloaded bytes against the digest
    # recorded from SHA2-256SUMS — there is deliberately no unverified path.
    download_and_verify(url, out, pins, artifact=ytdlp_pin_key(artifact, version))
    make_executable(out)


def find_one(root: Path, pattern: str) -> Path:
    matches = list(root.rglob(pattern))
    files = [p for p in matches if p.is_file()]
    if not files:
        die(f"binary not found matching {pattern!r} under {root}")
    return files[0]


def extract_tar_xz(archive: Path, dest: Path) -> None:
    with tarfile.open(archive, mode="r:xz") as tf:
        # Passing "data" explicitly makes the path-traversal guard visible and
        # gives 3.12 through 3.14 one extraction behavior (3.12-3.13 default to
        # fully_trusted, 3.14+ to data). The kwarg exists on 3.12+ and on the
        # 3.9.17/3.10.12/3.11.4 backports; older interpreters fail loudly
        # instead of silently extracting unfiltered.
        tf.extractall(dest, filter="data")


def fetch_ffmpeg(system: str, machine: str, out: Path, pins: dict) -> None:
    print(f"Downloading ffmpeg {FFMPEG_VERSION}...")
    # In --update-pins `pins` is the PinsRecorder: resolve the asset name
    # against its table, which holds this run's pruned superseded keys and
    # newly recorded names. The on-disk file still has the old state.
    downloads = pins.downloads if isinstance(pins, PinsRecorder) else pins["downloads"]
    # A private work dir per call: --update-pins extracts five platforms in
    # one process, and a shared dir would let one platform's leftovers be
    # picked up by the next.
    work = Path(tempfile.mkdtemp(prefix="videofetch-ffmpeg-"))
    try:
        if system == "Darwin":
            zip_path = work / "ffmpeg.zip"
            download_and_verify(
                ffmpeg_macos_url(), zip_path, pins, artifact=ffmpeg_macos_artifact()
            )
            extract_dir = work / "extract"
            extract_dir.mkdir()
            with zipfile.ZipFile(zip_path) as zf:
                zf.extractall(extract_dir)
            src = extract_dir / "ffmpeg"
            if not src.is_file():
                src = find_one(extract_dir, "ffmpeg")
            shutil.copy2(src, out)
            make_executable(out)
            verify_written(out, verify_macos_ffmpeg)
            return

        if system == "Linux":
            try:
                url = ffmpeg_download_url(system, machine, downloads)
            except ValueError as e:
                die(str(e))
            assert url is not None
            archive = work / "ffmpeg.tar.xz"
            download_and_verify(url, archive, pins)
            extract_dir = work / "ffmpeg-linux"
            extract_dir.mkdir()
            extract_tar_xz(archive, extract_dir)
            # Prefer */bin/ffmpeg like the bash find path filter.
            candidates = [
                p
                for p in extract_dir.rglob("ffmpeg")
                if p.is_file() and p.parent.name == "bin"
            ]
            if not candidates:
                die("ffmpeg binary not found in archive")
            shutil.copy2(candidates[0], out)
            make_executable(out)
            verify_written(out, verify_btbn_ffmpeg_arch, system, machine)
            return

        if system == "Windows":
            try:
                url = ffmpeg_download_url(system, machine, downloads)
            except ValueError as e:
                die(str(e))
            assert url is not None
            zip_path = work / "ffmpeg.zip"
            download_and_verify(url, zip_path, pins)
            with zipfile.ZipFile(zip_path) as zf:
                zf.extractall(work)
            src = find_one(work, "ffmpeg.exe")
            shutil.copy2(src, out)
            verify_written(out, verify_btbn_ffmpeg_arch, system, machine)
            return

        die(f"Unsupported OS for ffmpeg: {system}")
    finally:
        shutil.rmtree(work, ignore_errors=True)


def parse_args(argv: list[str]) -> tuple[bool, bool]:
    update = "--update-pins" in argv
    force = "--force" in argv
    unknown = [a for a in argv if a not in {"--update-pins", "--force"}]
    if unknown:
        die(f"unknown argument(s): {' '.join(unknown)}\n\n{USAGE}")
    if force and not update:
        die(f"--force is only valid together with --update-pins\n\n{USAGE}")
    return update, force


def select_btbn_assets(shasums: dict[str, str]) -> list[str]:
    """Pinned-snapshot assets for every platform in the release matrix."""
    branch = ffmpeg_branch()
    selected: list[str] = []
    for system, arches in BTBN_ARCHES.items():
        ext = "zip" if system == "Windows" else "tar.xz"
        for arch in sorted(set(arches.values())):
            names = [
                name
                for name in shasums
                if name.startswith("ffmpeg-n")
                and name.endswith(f"-{arch}-gpl-{branch}.{ext}")
                and "-shared-" not in name
            ]
            if len(names) != 1:
                die(
                    f"expected exactly one BtbN asset for {arch} on branch "
                    f"{branch}, found {sorted(names)}; check FFMPEG_BTBN_TAG "
                    "and FFMPEG_VERSION against the snapshot"
                )
            selected.append(names[0])
    return selected


ALL_PLATFORMS = (
    ("Darwin", "arm64"),
    ("Linux", "x86_64"),
    ("Linux", "aarch64"),
    ("Windows", "x86_64"),
    ("Windows", "ARM64"),
)

# Host triples as the release workflow's runners report them; used as keys
# for the extracted-binary digests.
PLATFORM_TRIPLES = {
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
    ("Linux", "aarch64"): "aarch64-unknown-linux-gnu",
    ("Windows", "x86_64"): "x86_64-pc-windows-msvc",
    ("Windows", "ARM64"): "aarch64-pc-windows-msvc",
}


class PinsRecorder:
    """`--update-pins` policy: record new digests, never overwrite silently.

    A digest that differs from the file is only replaced when --force is
    passed, so a mismatch cannot be cleared by re-running the update; stale
    stays set and the run fails at the end with instructions. Filenames that
    embed a version (the macOS zip, BtbN's build-tagged assets) are replaced
    wholesale when the build is bumped — update_pins prunes the superseded
    keys — which is why a missing entry is recorded without --force; yt-dlp's
    names carry no version, so its pin key appends the release tag for the
    same reason.

    Digests recorded from an upstream checksum file also pin every later
    download of that artifact for the rest of the run: what gets extracted
    and executed must be the same bytes the upstream file described.
    """

    def __init__(self, pins: dict, force: bool) -> None:
        self.pins = pins
        self.force = force
        self.stale = False
        self.upstream_digests: dict[str, str] = {}

    @property
    def downloads(self) -> dict[str, str]:
        """The run's downloads table, for asset names resolved mid-run."""
        return self.pins["downloads"]

    def _apply(self, table: dict, name: str, digest: str, note: str = "") -> None:
        old = table.get(name)
        if old == digest:
            print(f"  ok       {name}{note}")
        elif old is None:
            print(f"  pin new  {name}{note}")
            table[name] = digest
        elif self.force:
            print(f"  REPIN    {name}{note}\n    {old} -> {digest}")
            table[name] = digest
        else:
            self.stale = True
            print(
                f"  CHANGED  {name}{note}\n    {old}\n    {digest}\n"
                "    inspect the upstream change; to accept it, pass --force "
                "(or delete the entry and re-run)"
            )

    def record(self, artifact: str, digest: str, note: str = "") -> None:
        self.upstream_digests[artifact] = digest
        self._apply(self.pins["downloads"], artifact, digest, note)

    def download_and_record(
        self, url: str, dest: Path, artifact: str | None = None
    ) -> None:
        """Download the artifact and record its digest in the pins file.

        In the --update-pins flow the digest is recorded from upstream's
        published checksum before any download, and `download_and_verify`
        skips its usual check in recorder mode — so this guard is what keeps
        the archive that gets extracted and executed in sync with the digest
        that gets pinned. A mismatch is an incident, not a pin to refresh.
        """
        artifact = artifact or pin_key_from_url(url)
        download(url, dest)
        digest = sha256_file(dest)
        expected = self.upstream_digests.get(artifact)
        if expected is None:
            die_unverified(
                dest,
                f"no upstream digest was recorded for {artifact!r} before "
                f"downloading {url}; refusing to pin bytes no checksum "
                "file described",
            )
        if expected != digest:
            die_unverified(
                dest,
                f"{artifact} downloaded from {url} does not match the digest "
                "recorded from upstream earlier in this run:\n"
                f"  upstream  {expected}\n"
                f"  served    {digest}\n"
                "Treat this as a supply-chain incident: the pin and the "
                "archive that is about to be extracted disagree.",
            )
        self.record(artifact, digest, f" ({dest.stat().st_size} bytes)")

    def record_binary(self, kind: str, triple: str, out: Path) -> None:
        self._apply(self.pins["binaries"].setdefault(kind, {}), triple, sha256_file(out))
        out.unlink()

    def write(self) -> bool:
        write_pins(self.pins)
        return self.stale


def update_pins(force: bool) -> None:
    """Refresh scripts/sidecar_pins.json from the pinned upstream releases.

    Digests come from the checksum files yt-dlp, BtbN and martin-riedl
    publish for the pinned builds; the macOS zip is cross-checked against
    upstream's .sha256 before its pin is recorded. Every platform's archive
    is also downloaded once to record the extracted binary's digest, which
    is what ordinary builds check cached binaries against.
    """
    pins = load_pins()
    recorder = PinsRecorder(pins, force)

    if FFMPEG_BTBN_TAG == "latest":
        die("FFMPEG_BTBN_TAG must be a frozen autobuild tag, not 'latest'")

    ytdlp_version = load_ytdlp_version()
    print(f"yt-dlp {ytdlp_version} — digests from SHA2-256SUMS")
    ytdlp_sums = ytdlp_upstream_shasums(ytdlp_version)
    for system, machine in ALL_PLATFORMS:
        artifact = ytdlp_asset_name(system, machine)
        if artifact not in ytdlp_sums:
            die(f"{artifact} missing from yt-dlp {ytdlp_version} SHA2-256SUMS")
        recorder.record(ytdlp_pin_key(artifact, ytdlp_version), ytdlp_sums[artifact])

    # The versioned keys are namespaced by release tag, so a bump leaves the
    # superseded release's keys behind; the ffmpeg pass prunes its own
    # superseded keys below, once the pinned snapshot is known.
    current = {
        ytdlp_pin_key(ytdlp_asset_name(system, machine), ytdlp_version)
        for system, machine in ALL_PLATFORMS
    }
    asset_names = {key.split("@", 1)[0] for key in current}
    superseded = [
        n
        for n in pins["downloads"]
        if n.split("@", 1)[0] in asset_names and n not in current
    ]
    for name in superseded:
        del pins["downloads"][name]
        print(f"  pruned   {name} (superseded by yt-dlp {ytdlp_version})")
    if superseded:
        # binaries.yt-dlp holds the same bytes as the versioned downloads,
        # keyed by target triple: left in place, a bump would land as
        # CHANGED under unchanged keys and demand --force — the routine the
        # versioned keys exist to eliminate. The recording loop below
        # re-adds the triples from the new release.
        dropped = pins["binaries"].pop("yt-dlp", {})
        if dropped:
            print(
                f"  pruned   binaries.yt-dlp ({len(dropped)} triples, "
                f"re-recorded from yt-dlp {ytdlp_version} below)"
            )

    print(
        f"ffmpeg {FFMPEG_VERSION} branch {ffmpeg_branch()} — "
        f"digests from BtbN {FFMPEG_BTBN_TAG}/checksums.sha256"
    )
    btbn_sums = btbn_upstream_shasums()
    current_btbn = select_btbn_assets(btbn_sums)
    for artifact in current_btbn:
        recorder.record(artifact, btbn_sums[artifact])

    macos_zip = ffmpeg_macos_artifact()
    # Same policy as the yt-dlp pass: a build bump rewrites the names the
    # keys carry (BtbN embeds the build, the macOS key carries the build id),
    # so the superseded build's keys would linger. Left in place they make
    # btbn_asset_name ambiguous for the recording loop, and a stale
    # binaries.ffmpeg would turn that loop's re-record into CHANGED/--force.
    # The loop resolves names against the recorder's table and re-adds the
    # triples from the pinned build.
    superseded_ffmpeg = [
        name
        for name in pins["downloads"]
        if name.startswith(("ffmpeg-n", "ffmpeg-macos-"))
        and name not in {*current_btbn, macos_zip}
    ]
    for name in superseded_ffmpeg:
        del pins["downloads"][name]
        print(f"  pruned   {name} (superseded by the pinned ffmpeg build)")
    if superseded_ffmpeg:
        dropped = pins["binaries"].pop("ffmpeg", {})
        if dropped:
            print(
                f"  pruned   binaries.ffmpeg ({len(dropped)} triples, "
                "re-recorded from the pinned build below)"
            )

    sha_url = f"{ffmpeg_macos_url()}.sha256"
    print(f"martin-riedl macOS build {FFMPEG_MACOS_BUILD} — digest from {sha_url}")
    published = parse_shasums(fetch_bytes(sha_url).decode("utf-8"))
    if "ffmpeg.zip" not in published:
        die(f"{sha_url} does not list ffmpeg.zip")
    with tempfile.TemporaryDirectory(prefix="videofetch-pins-") as tmp_s:
        zip_path = Path(tmp_s) / "ffmpeg.zip"
        download(ffmpeg_macos_url(), zip_path)
        digest = sha256_file(zip_path)
        verify_sha256(
            "the macOS zip (upstream .sha256)", published["ffmpeg.zip"], digest
        )
        recorder.record(macos_zip, digest, f" ({zip_path.stat().st_size} bytes)")

    print("recording extracted binary digests (downloads every platform's archive)")
    with tempfile.TemporaryDirectory(prefix="videofetch-pins-") as tmp_s:
        tmp = Path(tmp_s)
        for index, (sysname, mach) in enumerate(ALL_PLATFORMS):
            triple = PLATFORM_TRIPLES[(sysname, mach)]
            ytdlp_out = tmp / f"yt-dlp-{index}"
            ffmpeg_out = tmp / f"ffmpeg-{index}"
            fetch_ytdlp(sysname, mach, ytdlp_out, ytdlp_version, recorder)
            recorder.record_binary("yt-dlp", triple, ytdlp_out)
            fetch_ffmpeg(sysname, mach, ffmpeg_out, recorder)
            recorder.record_binary("ffmpeg", triple, ffmpeg_out)

    stale = recorder.write()
    if stale:
        die(
            "existing pins differ from upstream (see CHANGED above). If the "
            "change is legitimate, re-run with --force; otherwise treat it as "
            "a supply-chain incident and investigate."
        )
    print("Pins up to date.")


def main() -> None:
    bin_dir = SIDECAR_DIR
    bin_dir.mkdir(parents=True, exist_ok=True)

    pins = load_pins()
    ytdlp_version = load_ytdlp_version()
    print(f"yt-dlp version: {ytdlp_version} (pinned in requirements-sidecars.txt)")
    print(
        f"ffmpeg version: {FFMPEG_VERSION} (pinned as FFMPEG_VERSION, "
        f"BtbN snapshot {FFMPEG_BTBN_TAG})"
    )

    triple = host_triple()
    system = detect_system()
    machine = detect_machine()
    if system == "Darwin" and machine not in {"arm64", "aarch64"}:
        die(
            f"no ffmpeg build for macOS/{machine}: the pinned macOS sidecar "
            "is arm64-only (Apple silicon)"
        )
    ext = ".exe" if system == "Windows" else ""

    ytdlp_out = bin_dir / f"yt-dlp-{triple}{ext}"
    ffmpeg_out = bin_dir / f"ffmpeg-{triple}{ext}"

    # Checked after the paths are known, not at load time: a mismatch
    # invalidates the cached ffmpeg binary, and those bytes must not stay at
    # the sidecar path for the next `tauri build` to bundle.
    try:
        verify_ffmpeg_snapshot(pins)
    except SystemExit:
        ffmpeg_out.unlink(missing_ok=True)
        raise

    print(f"Fetching sidecars for {triple} ({system}/{machine}) -> {bin_dir}")

    # A cached binary is reused only if it still matches its pinned digest;
    # anything else, a missing pin included, is re-fetched, re-verified and
    # removed first so a failed re-fetch cannot leave it behind. yt-dlp's
    # binary pin is keyed by target triple and survives a version bump on
    # its own, so it is checked against the pinned release as well.
    need_ytdlp = ytdlp_needs_fetch(
        ytdlp_out, pins, triple, system, machine, ytdlp_version
    )
    need_ffmpeg = ffmpeg_needs_fetch(ffmpeg_out, pins, triple)

    if not need_ytdlp and not need_ffmpeg:
        print("Sidecars already present and verified, skipping download:")
        for p in (ytdlp_out, ffmpeg_out):
            print(f"  {p} ({p.stat().st_size} bytes)")
        return

    if need_ytdlp:
        fetch_ytdlp(system, machine, ytdlp_out, ytdlp_version, pins)
    else:
        print(f"Keeping cached yt-dlp (digest verified): {ytdlp_out}")
    if need_ffmpeg:
        fetch_ffmpeg(system, machine, ffmpeg_out, pins)
    else:
        print(f"Keeping cached ffmpeg (digest verified): {ffmpeg_out}")

    print("Sidecars ready:")
    for p in (ytdlp_out, ffmpeg_out):
        size = p.stat().st_size
        print(f"  {p} ({size} bytes)")


def _needs_fetch(path: Path, expected: str | None, kind: str) -> bool:
    if not path.is_file() or path.stat().st_size == 0:
        return True
    if expected is None:
        print(f"no pinned digest for {kind} {path.name}; re-fetching to verify")
        return True
    if sha256_file(path).lower() != expected.lower():
        print(f"cached {kind} {path.name} does not match its pinned digest; re-fetching")
        return True
    return False


def ytdlp_needs_fetch(
    out: Path, pins: dict, triple: str, system: str, machine: str, version: str
) -> bool:
    """Whether the cached yt-dlp binary at `out` must be (re-)fetched.

    `binaries.yt-dlp` is keyed by target triple, so on its own it cannot tell
    that requirements-sidecars.txt was bumped: the cache still matches the
    superseded release's pin. A cached binary is trusted only when the
    versioned downloads pin for the *pinned release* agrees with it; a
    mismatch is re-fetched, which dies loudly on the missing versioned pin
    instead of silently bundling the old release from a warm cache.
    """
    expected = pins["binaries"].get("yt-dlp", {}).get(triple)
    if _needs_fetch(out, expected, "yt-dlp"):
        return True
    try:
        asset = ytdlp_asset_name(system, machine)
    except ValueError as e:
        die(str(e))
    release = pins["downloads"].get(ytdlp_pin_key(asset, version))
    if release is None:
        print(
            f"no pinned digest for yt-dlp {version}'s {asset}; "
            "re-fetching to verify"
        )
        return True
    if release != expected:
        print(
            f"cached yt-dlp {out.name} was pinned for another release than "
            f"yt-dlp {version}; re-fetching"
        )
        return True
    return False


def ffmpeg_needs_fetch(out: Path, pins: dict, triple: str) -> bool:
    """Whether the cached ffmpeg binary at `out` must be (re-)fetched.

    Same fail-closed rule as the yt-dlp cache: the binary is trusted only
    when it matches `binaries.ffmpeg[triple]`, and anything else — a digest
    that disagrees, no pin at all — is deleted before the re-fetch, so a
    failed re-fetch cannot leave it at the final sidecar path. The release
    identity that would make a stale binary look valid is covered separately
    by `verify_ffmpeg_snapshot`.
    """
    expected = pins["binaries"].get("ffmpeg", {}).get(triple)
    untrusted = _needs_fetch(out, expected, "ffmpeg")
    if untrusted:
        out.unlink(missing_ok=True)
    return untrusted


if __name__ == "__main__":
    update, force = parse_args(sys.argv[1:])
    if update:
        update_pins(force)
    else:
        main()
