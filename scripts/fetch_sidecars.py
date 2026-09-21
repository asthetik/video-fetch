#!/usr/bin/env python3
"""Download yt-dlp + ffmpeg into src-tauri/binaries/ for Tauri externalBin.

Versions are pinned, not floating:
  yt-dlp  — scripts/requirements-sidecars.txt (`yt-dlp==<PyPI version>`,
            auto-upgraded by Dependabot; zero-padded back to the GitHub
            release tag here)
  ffmpeg  — FFMPEG_VERSION + FFMPEG_BTBN_TAG below: a specific BtbN
            autobuild snapshot for Linux/Windows, evermeet x.y.z build for
            macOS; no package registry exists for Dependabot to track, so
            bump it manually

Integrity:
  Every downloaded artifact is checked against a SHA-256 digest pinned in
  scripts/sidecar_pins.json BEFORE it is extracted. The BtbN asset names
  and their digests are stored per snapshot, so the mutable `latest` tag
  is never used and an upstream rebuild fails the build instead of
  silently shipping different bytes.

  To bump yt-dlp: let Dependabot edit requirements-sidecars.txt, then add
  the new artifacts' digests:
      python scripts/fetch_sidecars.py --update-pins
  To bump ffmpeg: set FFMPEG_VERSION and FFMPEG_BTBN_TAG, then run the
  same --update-pins. New assets are appended; existing digests are never
  overwritten unless --force is passed, so a digest failure cannot be
  waved away by re-running the update (a human must look at the diff).
  yt-dlp and BtbN publish checksum files that --update-pins cross-checks;
  evermeet publishes none, so its macOS zip is pinned on first sight.
  Run scripts/verify_evermeet.py out-of-band to confirm that zip is really
  evermeet's (PGP signature) and still matches its pin.

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
from pathlib import Path

YTDLP_DOWNLOAD_BASE = "https://github.com/yt-dlp/yt-dlp/releases/download"
FFMPEG_BTBN_TAG = "autobuild-2026-09-20-13-11"
FFMPEG_BTBN_BASE = (
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/" + FFMPEG_BTBN_TAG
)
FFMPEG_EVERMEET = "https://evermeet.cx/ffmpeg"

# FFmpeg release branch to ship (x.y.z). Linux/Windows use the pinned BtbN
# snapshot's matching release-branch builds (n<x.y>); macOS uses evermeet's
# x.y.z build. Bumped manually — see the module docstring.
FFMPEG_VERSION = "9.0.1"

REQUIREMENTS_FILE = Path(__file__).resolve().parent / "requirements-sidecars.txt"
PINS_FILE = Path(__file__).resolve().parent / "sidecar_pins.json"

USAGE = """usage: fetch_sidecars.py [--update-pins [--force]]

  (no flags)     download and integrity-check sidecars for this host
  --update-pins  refresh missing or changed digests in sidecar_pins.json
                 (downloads every platform's artifacts; never overwrites
                 an existing digest without --force)
"""


def die(msg: str, code: int = 1) -> None:
    print(msg, file=sys.stderr)
    raise SystemExit(code)


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
    if data.get("schema", 1) != 1:
        die(f"{file}: unsupported schema version {data.get('schema')!r}")
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
        "schema": 1,
        "//": (
            "SHA-256 pins for artifacts downloaded by fetch_sidecars.py. "
            "Update with --update-pins; see the script docstring."
        ),
        "downloads": dict(sorted(pins["downloads"].items())),
    }
    if pins.get("binaries"):
        payload["binaries"] = dict(sorted(pins["binaries"].items()))
    file.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {file}")


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


def fetch_bytes(url: str) -> bytes:
    print(f"  GET {url}")
    try:
        with urllib.request.urlopen(url) as resp:
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
        with urllib.request.urlopen(url) as resp, dest.open("wb") as f:
            shutil.copyfileobj(resp, f)
    except (urllib.error.URLError, OSError) as e:
        die(f"failed to download {url}: {e}")


def download_and_verify(url: str, dest: Path, pins: dict) -> None:
    """Download the artifact and record or check it against its digest.

    In update mode (`PinsRecorder`) the digest was recorded from the pinned
    release's published checksums a moment earlier; the download is compared
    against it before the entry is trusted.
    """
    if isinstance(pins, PinsRecorder):
        pins.download_and_record(url, dest)
        return
    artifact = pin_key_from_url(url)
    download(url, dest)
    verify_sha256(artifact, pinned_digest(pins, artifact), sha256_file(dest))


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
    """yt-dlp release asset for a platform (names are version-independent)."""
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


def btbn_asset_name(system: str, machine: str) -> str:
    """The pinned BtbN asset name for a platform.

    The snapshot is pinned in sidecar_pins.json by asset name (BtbN embeds
    the exact build, e.g. ffmpeg-n9.0.2-3-ga5923073bf-..., in the file name,
    which is why the frozen autobuild tag alone is not enough).
    """
    branch = ffmpeg_branch()
    try:
        arch = BTBN_ARCHES[system][machine.lower()]
    except KeyError:
        raise ValueError(f"Unsupported OS/arch for ffmpeg: {system}/{machine}")
    ext = "zip" if system == "Windows" else "tar.xz"
    names = [
        name
        for name in load_pins()["downloads"]
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


def ffmpeg_download_url(system: str, machine: str) -> str | None:
    """Return archive URL, or None for Darwin (evermeet handled separately)."""
    if system == "Darwin":
        return None
    return f"{FFMPEG_BTBN_BASE}/{btbn_asset_name(system, machine)}"


def fetch_ytdlp(
    system: str,
    machine: str,
    out: Path,
    version: str,
    pins: dict,
    recorder: "PinsRecorder | None" = None,
) -> None:
    print(f"Downloading yt-dlp {version}...")
    try:
        url = ytdlp_download_url(system, machine, version)
    except ValueError as e:
        die(str(e))
    if recorder is None:
        download_and_verify(url, out, pins)
    else:
        download(url, out)  # digest recorded in the update pass
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
    # A private work dir per call: --update-pins extracts five platforms in
    # one process, and a shared dir would let one platform's leftovers be
    # picked up by the next.
    work = Path(tempfile.mkdtemp(prefix="videofetch-ffmpeg-"))
    try:
        if system == "Darwin":
            zip_path = work / "ffmpeg.zip"
            download_and_verify(
                f"{FFMPEG_EVERMEET}/ffmpeg-{FFMPEG_VERSION}.zip", zip_path, pins
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
            return

        if system == "Linux":
            try:
                url = ffmpeg_download_url(system, machine)
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
            return

        if system == "Windows":
            try:
                url = ffmpeg_download_url(system, machine)
            except ValueError as e:
                die(str(e))
            assert url is not None
            zip_path = work / "ffmpeg.zip"
            download_and_verify(url, zip_path, pins)
            with zipfile.ZipFile(zip_path) as zf:
                zf.extractall(work)
            src = find_one(work, "ffmpeg.exe")
            shutil.copy2(src, out)
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
    embed a version (evermeet's zip, BtbN's build-tagged assets) are meant to
    be deleted and re-added when the version is bumped, which is why a
    missing entry is recorded without --force.
    """

    def __init__(self, pins: dict, force: bool) -> None:
        self.pins = pins
        self.force = force
        self.stale = False

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
        self._apply(self.pins["downloads"], artifact, digest, note)

    def download_and_record(self, url: str, dest: Path) -> None:
        """Download the artifact and record its digest in the pins file."""
        artifact = pin_key_from_url(url)
        download(url, dest)
        self.record(artifact, sha256_file(dest), f" ({dest.stat().st_size} bytes)")

    def record_binary(self, kind: str, triple: str, out: Path) -> None:
        self._apply(self.pins["binaries"].setdefault(kind, {}), triple, sha256_file(out))
        out.unlink()

    def write(self) -> bool:
        write_pins(self.pins)
        return self.stale


def update_pins(force: bool) -> None:
    """Refresh scripts/sidecar_pins.json from the pinned upstream releases.

    Digests come from the checksum files yt-dlp and BtbN publish for the
    pinned release; evermeet publishes none, so its zip is downloaded and
    pinned on first sight. Every platform's archive is also downloaded once
    to record the extracted binary's digest, which is what ordinary builds
    check cached binaries against.
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
        recorder.record(artifact, ytdlp_sums[artifact])

    print(
        f"ffmpeg {FFMPEG_VERSION} branch {ffmpeg_branch()} — "
        f"digests from BtbN {FFMPEG_BTBN_TAG}/checksums.sha256"
    )
    btbn_sums = btbn_upstream_shasums()
    for artifact in select_btbn_assets(btbn_sums):
        recorder.record(artifact, btbn_sums[artifact])

    evermeet = f"ffmpeg-{FFMPEG_VERSION}.zip"
    print("evermeet (macOS) publishes no checksum file; pinning the downloaded zip")
    with tempfile.TemporaryDirectory(prefix="videofetch-pins-") as tmp_s:
        zip_path = Path(tmp_s) / evermeet
        download(f"{FFMPEG_EVERMEET}/{evermeet}", zip_path)
        recorder.record(evermeet, sha256_file(zip_path), f" ({zip_path.stat().st_size} bytes)")

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
    root = Path(__file__).resolve().parent.parent
    bin_dir = root / "src-tauri" / "binaries"
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
    ext = ".exe" if system == "Windows" else ""

    ytdlp_out = bin_dir / f"yt-dlp-{triple}{ext}"
    ffmpeg_out = bin_dir / f"ffmpeg-{triple}{ext}"

    print(f"Fetching sidecars for {triple} ({system}/{machine}) -> {bin_dir}")

    # A cached binary is reused only if it still matches its pinned digest;
    # anything else, a missing pin included, is re-fetched and re-verified.
    cached_ytdlp = pins["binaries"].get("yt-dlp", {}).get(triple)
    cached_ffmpeg = pins["binaries"].get("ffmpeg", {}).get(triple)
    need_ytdlp = _needs_fetch(ytdlp_out, cached_ytdlp, "yt-dlp")
    need_ffmpeg = _needs_fetch(ffmpeg_out, cached_ffmpeg, "ffmpeg")

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


if __name__ == "__main__":
    update, force = parse_args(sys.argv[1:])
    if update:
        update_pins(force)
    else:
        main()
