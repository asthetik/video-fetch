#!/usr/bin/env python3
"""Download yt-dlp + ffmpeg into src-tauri/binaries/ for Tauri externalBin.

Naming:
  binaries/yt-dlp-{TARGET_TRIPLE}[.exe]
  binaries/ffmpeg-{TARGET_TRIPLE}[.exe]
TARGET_TRIPLE from: rustc --print host-tuple

Run before `npm run tauri build` (CI release) or local bundling.
"""

from __future__ import annotations

import platform
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path

YTDLP_LATEST = "https://github.com/yt-dlp/yt-dlp/releases/latest/download"
FFMPEG_LATEST = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest"


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


def ytdlp_download_url(system: str, machine: str) -> str:
    m = machine.lower()
    if system == "Darwin":
        return f"{YTDLP_LATEST}/yt-dlp_macos"
    if system == "Linux":
        if m in {"x86_64", "amd64"}:
            return f"{YTDLP_LATEST}/yt-dlp_linux"
        if m in {"aarch64", "arm64"}:
            return f"{YTDLP_LATEST}/yt-dlp_linux_aarch64"
        raise ValueError(f"Unsupported Linux arch for yt-dlp: {machine}")
    if system == "Windows":
        if m in {"aarch64", "arm64"}:
            return f"{YTDLP_LATEST}/yt-dlp_arm64.exe"
        if m in {"x86_64", "amd64", "x64"}:
            return f"{YTDLP_LATEST}/yt-dlp.exe"
        raise ValueError(f"Unsupported Windows arch for yt-dlp: {machine}")
    raise ValueError(f"Unsupported OS for yt-dlp: {system}")


def ffmpeg_download_url(system: str, machine: str) -> str | None:
    """Return archive URL, or None for Darwin (evermeet handled separately)."""
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
        return f"{FFMPEG_LATEST}/ffmpeg-master-latest-{arch}-gpl.tar.xz"
    if system == "Windows":
        if m in {"aarch64", "arm64"}:
            tag = "winarm64"
        elif m in {"x86_64", "amd64", "x64"}:
            tag = "win64"
        else:
            raise ValueError(f"Unsupported Windows arch for ffmpeg: {machine}")
        return f"{FFMPEG_LATEST}/ffmpeg-master-latest-{tag}-gpl.zip"
    raise ValueError(f"Unsupported OS for ffmpeg: {system}")


def fetch_ytdlp(system: str, machine: str, out: Path) -> None:
    print("Downloading yt-dlp...")
    try:
        url = ytdlp_download_url(system, machine)
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


def fetch_ffmpeg(system: str, machine: str, out: Path, tmp: Path) -> None:
    print("Downloading ffmpeg...")
    if system == "Darwin":
        zip_path = tmp / "ffmpeg.zip"
        download("https://evermeet.cx/ffmpeg/getrelease/zip", zip_path)
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
        with tarfile.open(archive, mode="r:xz") as tf:
            tf.extractall(tmp)
        # Prefer */bin/ffmpeg like the bash find path filter.
        candidates = [
            p for p in tmp.rglob("ffmpeg") if p.is_file() and p.parent.name == "bin"
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
            fetch_ytdlp(system, machine, ytdlp_out)
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
