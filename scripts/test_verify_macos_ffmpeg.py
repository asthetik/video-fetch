#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import io
import os
import sys
import unittest
import zipfile
from unittest import mock

import verify_macos_ffmpeg
from fetch_sidecars import ffmpeg_macos_artifact, ffmpeg_macos_url, load_pins

MACOS_TRIPLE = "aarch64-apple-darwin"


def arm64_ffmpeg_bytes() -> bytes:
    return (
        (0xFEEDFACF).to_bytes(4, "little")
        + (0x0100000C).to_bytes(4, "little")
        + b"\x00" * 64
    )


def zip_bytes_with_ffmpeg(payload: bytes) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr("ffmpeg", payload)
    return buf.getvalue()


class VerifyPipelineTest(unittest.TestCase):
    """Drives main() end to end with the network and Mach-O check stubbed."""

    def setUp(self) -> None:
        self.payload = arm64_ffmpeg_bytes()
        self.zip_bytes = zip_bytes_with_ffmpeg(self.payload)
        self.zip_sha = hashlib.sha256(self.zip_bytes).hexdigest()
        self.binary_sha = hashlib.sha256(self.payload).hexdigest()

    def install(
        self,
        *,
        pin_zip: str | None = None,
        pin_binary: str | None = None,
        served_zip: bytes | None = None,
        published_sha: str | None = None,
        pinned: bool = True,
    ) -> list[object]:
        pins: dict = {
            "downloads": {},
            "binaries": {"ffmpeg": {MACOS_TRIPLE: pin_binary or self.binary_sha}},
        }
        if pinned:
            pins["downloads"][ffmpeg_macos_artifact()] = pin_zip or self.zip_sha

        published = published_sha or self.zip_sha
        served = served_zip if served_zip is not None else self.zip_bytes

        def fake_fetch_bytes(url: str) -> bytes:
            self.assertTrue(url.startswith("https://"), url)
            if url.endswith(".sha256"):
                return f"{published}  ffmpeg.zip\n".encode("utf-8")
            self.assertEqual(url, ffmpeg_macos_url())
            return served

        checked: list[tuple[object, bool]] = []

        def fake_verify(path: object) -> None:
            # Record executability at probe time; the temp dir is removed
            # before the test can inspect the path.
            checked.append((path, os.access(path, os.X_OK)))

        for name, replacement in (
            ("load_pins", lambda: pins),
            ("fetch_bytes", fake_fetch_bytes),
            ("verify_macos_ffmpeg", fake_verify),
        ):
            patcher = mock.patch.object(verify_macos_ffmpeg, name, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)
        return checked

    def test_checks_the_full_chain(self) -> None:
        checked = self.install()
        self.assertIsNone(verify_macos_ffmpeg.main())
        self.assertEqual(len(checked), 1, "Mach-O check must run on the binary")

    @unittest.skipIf(sys.platform == "win32", "chmod does not apply on Windows")
    def test_probed_binary_is_executable(self) -> None:
        """zipfile does not restore permission bits; the probe execs the file."""
        checked = self.install()
        self.assertIsNone(verify_macos_ffmpeg.main())
        self.assertTrue(
            checked[0][1],
            "extracted binary must be executable before the -version probe",
        )

    def test_zip_without_ffmpeg_dies(self) -> None:
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as zf:
            zf.writestr("readme.txt", b"not ffmpeg")
        payload = buf.getvalue()
        sha = hashlib.sha256(payload).hexdigest()
        with self.assertRaises(SystemExit):
            self.install(pin_zip=sha, published_sha=sha, served_zip=payload)
            verify_macos_ffmpeg.main()

    def test_upstream_checksum_mismatch_is_an_incident(self) -> None:
        checked = self.install(published_sha="0" * 64)
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg.main()
        self.assertEqual(checked, [])

    def test_download_mismatch_dies_before_extracting(self) -> None:
        checked = self.install(served_zip=b"not the pinned build")
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg.main()
        self.assertEqual(checked, [])

    def test_binary_mismatch_dies(self) -> None:
        with self.assertRaises(SystemExit):
            self.install(pin_binary="1" * 64)
            verify_macos_ffmpeg.main()

    def test_unpinned_artifact_asks_for_update_pins(self) -> None:
        self.install(pinned=False)
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg.main()

    def test_missing_binary_pin_dies(self) -> None:
        pins = {"downloads": {ffmpeg_macos_artifact(): self.zip_sha}, "binaries": {}}
        patcher = mock.patch.object(verify_macos_ffmpeg, "load_pins", lambda: pins)
        patcher.start()
        self.addCleanup(patcher.stop)
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg.main()


class PinsFileTest(unittest.TestCase):
    def test_the_pinned_build_is_in_the_pin_file(self) -> None:
        pins = load_pins()
        self.assertIn(ffmpeg_macos_artifact(), pins["downloads"])
        self.assertIn(MACOS_TRIPLE, pins["binaries"]["ffmpeg"])


if __name__ == "__main__":
    unittest.main()
