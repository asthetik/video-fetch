#!/usr/bin/env python3
"""Re-check out-of-band that the pinned macOS ffmpeg zip is what we think.

fetch_sidecars.py checks the zip against its digest in sidecar_pins.json
and asserts the extracted binary is a signed arm64 Mach-O at fetch time.
This script re-runs that chain against upstream from scratch, as a
supply-chain audit:

  upstream    the .sha256 martin-riedl publishes next to the pinned build
              (an immutable build-id URL, never /redirect/latest/...)
              must equal the digest pinned in scripts/sidecar_pins.json
  integrity   the downloaded zip must hash to that same digest, and the
              extracted binary must match the binaries pin for
              aarch64-apple-darwin
  binary      the extracted binary must be an arm64 Mach-O and, on macOS,
              carry a Developer ID signature chain from Martin Riedl up
              to Apple Root CA

Downloads ~28 MB into a temp dir that is removed on exit.

    python scripts/verify_macos_ffmpeg.py

Prints OK and exits 0 when every check holds; dies on the first failure.
"""

from __future__ import annotations

import sys
import tempfile
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from fetch_sidecars import (  # noqa: E402
    PINS_FILE,
    die,
    fetch_bytes,
    ffmpeg_macos_artifact,
    ffmpeg_macos_url,
    load_pins,
    make_executable,
    parse_shasums,
    sha256_file,
    verify_macos_ffmpeg,
)

MACOS_TRIPLE = "aarch64-apple-darwin"


def main() -> None:
    pins = load_pins()
    artifact = ffmpeg_macos_artifact()
    expected = pins["downloads"].get(artifact)
    if expected is None:
        die(
            f"{artifact} is not pinned in {PINS_FILE.name}; refresh it with\n"
            "  python scripts/fetch_sidecars.py --update-pins"
        )
    binary_pin = pins["binaries"].get("ffmpeg", {}).get(MACOS_TRIPLE)
    if binary_pin is None:
        die(f"no binaries.ffmpeg.{MACOS_TRIPLE} pin in {PINS_FILE.name}")

    sha_url = f"{ffmpeg_macos_url()}.sha256"
    published = parse_shasums(fetch_bytes(sha_url).decode("utf-8"))
    if published.get("ffmpeg.zip") != expected:
        die(
            f"upstream's {sha_url}\n  lists    {published.get('ffmpeg.zip')}\n"
            f"but {PINS_FILE.name} pins\n  {expected}\n"
            "The pinned build URL is immutable, so upstream is serving "
            "different bytes than the pin: treat that as a supply-chain "
            "incident rather than as a pin to refresh."
        )
    print("upstream .sha256 matches the pin")

    with tempfile.TemporaryDirectory(prefix="macos-ffmpeg-verify-") as tmp_s:
        work = Path(tmp_s)
        print(f"downloading {artifact} ...")
        zip_path = work / "ffmpeg.zip"
        zip_path.write_bytes(fetch_bytes(ffmpeg_macos_url()))

        actual = sha256_file(zip_path)
        if actual != expected:
            die(
                f"SHA-256 mismatch for {artifact}:\n"
                f"  pinned  {expected}\n"
                f"  served  {actual}\n"
                "Upstream's published checksum and the pin agree, so the "
                "download itself is suspect; retry before treating it as an "
                "incident."
            )
        print(f"zip digest matches scripts/{PINS_FILE.name}")

        extract_dir = work / "extract"
        extract_dir.mkdir()
        with zipfile.ZipFile(zip_path) as zf:
            zf.extractall(extract_dir)
        binary = extract_dir / "ffmpeg"
        if not binary.is_file():
            die("ffmpeg not found in the zip")
        # zipfile does not restore permission bits; the exec probe below
        # needs the extracted binary to be executable.
        make_executable(binary)

        verify_macos_ffmpeg(binary)

        binary_actual = sha256_file(binary)
        if binary_actual != binary_pin:
            die(
                "SHA-256 mismatch for the extracted ffmpeg binary:\n"
                f"  pinned  {binary_pin}\n"
                f"  actual  {binary_actual}"
            )
        print(f"binary digest matches binaries.ffmpeg.{MACOS_TRIPLE}")

    print("OK")


if __name__ == "__main__":
    main()
