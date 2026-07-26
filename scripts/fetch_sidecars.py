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


def fetch_ytdlp(system: str, out: Path) -> None:
    print("Downloading yt-dlp...")
    if system == "Darwin":
        url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos"
    elif system == "Linux":
        url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp"
    elif system == "Windows":
        url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe"
    else:
        die(f"Unsupported OS for yt-dlp: {system}")
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
        if machine in {"x86_64", "amd64"}:
            arch = "linux64"
        elif machine in {"aarch64", "arm64"}:
            arch = "linuxarm64"
        else:
            die(f"Unsupported Linux arch for ffmpeg: {machine}")
        archive = tmp / "ffmpeg.tar.xz"
        url = (
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/"
            f"ffmpeg-master-latest-{arch}-gpl.tar.xz"
        )
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
        zip_path = tmp / "ffmpeg.zip"
        url = (
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/"
            "ffmpeg-master-latest-win64-gpl.zip"
        )
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

    print(f"Fetching sidecars for {triple} ({system}/{machine}) → {bin_dir}")

    with tempfile.TemporaryDirectory(prefix="videofetch-sidecars-") as tmp_s:
        tmp = Path(tmp_s)
        fetch_ytdlp(system, ytdlp_out)
        fetch_ffmpeg(system, machine, ffmpeg_out, tmp)

    print("Sidecars ready:")
    for p in (ytdlp_out, ffmpeg_out):
        size = p.stat().st_size
        print(f"  {p} ({size} bytes)")


if __name__ == "__main__":
    main()
