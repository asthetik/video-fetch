#!/usr/bin/env python3
"""Download yt-dlp + ffmpeg into src-tauri/binaries/ for Tauri externalBin.

Versions are pinned, not floating:
  yt-dlp  — scripts/requirements-sidecars.txt (`yt-dlp==<PyPI version>`,
            auto-upgraded by Dependabot; zero-padded back to the GitHub
            release tag here)
  ffmpeg  — FFMPEG_VERSION below: BtbN release-branch builds (n<x.y>) for
            Linux/Windows, evermeet x.y.z build for macOS; no package
            registry exists for Dependabot to track, so bump it manually

Naming:
  binaries/yt-dlp-{TARGET_TRIPLE}[.exe]
  binaries/ffmpeg-{TARGET_TRIPLE}[.exe]
TARGET_TRIPLE from: rustc --print host-tuple

Run before `npm run tauri build` (CI release) or local bundling.
"""

from __future__ import annotations

import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

YTDLP_DOWNLOAD_BASE = "https://github.com/yt-dlp/yt-dlp/releases/download"
FFMPEG_BTBN_LATEST = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest"
FFMPEG_EVERMEET = "https://evermeet.cx/ffmpeg"

# FFmpeg release to ship (x.y.z). Linux/Windows use BtbN builds of the
# matching release branch (n<x.y>), macOS uses evermeet's x.y.z build.
# Bumped manually — see requirements-sidecars.txt header for why.
FFMPEG_VERSION = "9.0.1"

REQUIREMENTS_FILE = Path(__file__).resolve().parent / "requirements-sidecars.txt"


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


def download(url: str, dest: Path) -> None:
    print(f"  GET {url}")
    with urllib.request.urlopen(url) as resp, dest.open("wb") as f:
        shutil.copyfileobj(resp, f)


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


def ytdlp_download_url(system: str, machine: str, version: str) -> str:
    base = f"{YTDLP_DOWNLOAD_BASE}/{version}"
    m = machine.lower()
    if system == "Darwin":
        return f"{base}/yt-dlp_macos"
    if system == "Linux":
        if m in {"x86_64", "amd64"}:
            return f"{base}/yt-dlp_linux"
        if m in {"aarch64", "arm64"}:
            return f"{base}/yt-dlp_linux_aarch64"
        raise ValueError(f"Unsupported Linux arch for yt-dlp: {machine}")
    if system == "Windows":
        if m in {"aarch64", "arm64"}:
            return f"{base}/yt-dlp_arm64.exe"
        if m in {"x86_64", "amd64", "x64"}:
            return f"{base}/yt-dlp.exe"
        raise ValueError(f"Unsupported Windows arch for yt-dlp: {machine}")
    raise ValueError(f"Unsupported OS for yt-dlp: {system}")


def ffmpeg_branch(version: str = FFMPEG_VERSION) -> str:
    """Release branch of an x.y.z version (9.0.1 -> 9.0), as used by BtbN."""
    parts = version.split(".")
    if len(parts) != 3 or any(not p.isdigit() for p in parts):
        die(f"FFMPEG_VERSION {version!r} must be a full x.y.z release like 9.0.1")
    return ".".join(parts[:2])


def ffmpeg_download_url(system: str, machine: str) -> str | None:
    """Return archive URL, or None for Darwin (evermeet handled separately)."""
    branch = ffmpeg_branch()
    m = machine.lower()
    if system == "Darwin":
        return None
    if system == "Linux":
        if m in {"x86_64", "amd64"}:
            arch = "linux64"
        elif m in {"aarch64", "arm64"}:
            arch = "linuxarm64"
        else:
            raise ValueError(f"Unsupported Linux arch for ffmpeg: {machine}")
        return f"{FFMPEG_BTBN_LATEST}/ffmpeg-n{branch}-latest-{arch}-gpl-{branch}.tar.xz"
    if system == "Windows":
        if m in {"aarch64", "arm64"}:
            tag = "winarm64"
        elif m in {"x86_64", "amd64", "x64"}:
            tag = "win64"
        else:
            raise ValueError(f"Unsupported Windows arch for ffmpeg: {machine}")
        return f"{FFMPEG_BTBN_LATEST}/ffmpeg-n{branch}-latest-{tag}-gpl-{branch}.zip"
    raise ValueError(f"Unsupported OS for ffmpeg: {system}")


def fetch_ytdlp(system: str, machine: str, out: Path, version: str) -> None:
    print(f"Downloading yt-dlp {version}...")
    try:
        url = ytdlp_download_url(system, machine, version)
    except ValueError as e:
        die(str(e))
    download(url, out)
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


def fetch_ffmpeg(system: str, machine: str, out: Path, tmp: Path) -> None:
    print(f"Downloading ffmpeg {FFMPEG_VERSION}...")
    if system == "Darwin":
        zip_path = tmp / "ffmpeg.zip"
        download(f"{FFMPEG_EVERMEET}/ffmpeg-{FFMPEG_VERSION}.zip", zip_path)
        extract_dir = tmp / "ffmpeg-macos"
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
        archive = tmp / "ffmpeg.tar.xz"
        download(url, archive)
        extract_dir = tmp / "ffmpeg-linux"
        extract_dir.mkdir()
        extract_tar_xz(archive, extract_dir)
        # Prefer */bin/ffmpeg like the bash find path filter.
        candidates = [
            p for p in extract_dir.rglob("ffmpeg")
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
        zip_path = tmp / "ffmpeg.zip"
        download(url, zip_path)
        extract_dir = tmp / "ffmpeg-win"
        extract_dir.mkdir()
        with zipfile.ZipFile(zip_path) as zf:
            zf.extractall(extract_dir)
        src = find_one(extract_dir, "ffmpeg.exe")
        shutil.copy2(src, out)
        return

    die(f"Unsupported OS for ffmpeg: {system}")


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    bin_dir = root / "src-tauri" / "binaries"
    bin_dir.mkdir(parents=True, exist_ok=True)

    ytdlp_version = load_ytdlp_version()
    print(f"yt-dlp version: {ytdlp_version} (pinned in requirements-sidecars.txt)")
    print(f"ffmpeg version: {FFMPEG_VERSION} (pinned as FFMPEG_VERSION)")

    triple = host_triple()
    system = detect_system()
    machine = detect_machine()
    ext = ".exe" if system == "Windows" else ""

    ytdlp_out = bin_dir / f"yt-dlp-{triple}{ext}"
    ffmpeg_out = bin_dir / f"ffmpeg-{triple}{ext}"

    print(f"Fetching sidecars for {triple} ({system}/{machine}) -> {bin_dir}")

    need_ytdlp = not ytdlp_out.is_file() or ytdlp_out.stat().st_size == 0
    need_ffmpeg = not ffmpeg_out.is_file() or ffmpeg_out.stat().st_size == 0

    if not need_ytdlp and not need_ffmpeg:
        print("Sidecars already present, skipping download:")
        for p in (ytdlp_out, ffmpeg_out):
            print(f"  {p} ({p.stat().st_size} bytes)")
        return

    with tempfile.TemporaryDirectory(prefix="videofetch-sidecars-") as tmp_s:
        tmp = Path(tmp_s)
        if need_ytdlp:
            fetch_ytdlp(system, machine, ytdlp_out, ytdlp_version)
        else:
            print(f"Keeping cached yt-dlp: {ytdlp_out}")
        if need_ffmpeg:
            fetch_ffmpeg(system, machine, ffmpeg_out, tmp)
        else:
            print(f"Keeping cached ffmpeg: {ffmpeg_out}")

    print("Sidecars ready:")
    for p in (ytdlp_out, ffmpeg_out):
        size = p.stat().st_size
        print(f"  {p} ({size} bytes)")


if __name__ == "__main__":
    main()
