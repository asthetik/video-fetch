#!/usr/bin/env python3
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from fetch_sidecars import (
    FFMPEG_VERSION,
    ffmpeg_branch,
    ffmpeg_download_url,
    load_ytdlp_version,
    normalize_ytdlp_version,
    ytdlp_download_url,
)

PIN = "2026.8.19"
TAG = "2026.08.19"


class TestYtdlpUrl(unittest.TestCase):
    def test_darwin(self) -> None:
        self.assertTrue(
            ytdlp_download_url("Darwin", "arm64", TAG)
            .endswith(f"/download/{TAG}/yt-dlp_macos")
        )

    def test_linux_x64(self) -> None:
        self.assertTrue(
            ytdlp_download_url("Linux", "x86_64", TAG)
            .endswith(f"/download/{TAG}/yt-dlp_linux")
        )

    def test_linux_arm64(self) -> None:
        self.assertTrue(
            ytdlp_download_url("Linux", "aarch64", TAG)
            .endswith(f"/download/{TAG}/yt-dlp_linux_aarch64")
        )

    def test_windows_x64(self) -> None:
        self.assertTrue(
            ytdlp_download_url("Windows", "AMD64", TAG)
            .endswith(f"/download/{TAG}/yt-dlp.exe")
        )

    def test_windows_arm64(self) -> None:
        self.assertTrue(
            ytdlp_download_url("Windows", "ARM64", TAG)
            .endswith(f"/download/{TAG}/yt-dlp_arm64.exe")
        )


class TestNormalizeYtdlpVersion(unittest.TestCase):
    def test_pypi_form_gets_padded(self) -> None:
        self.assertEqual(normalize_ytdlp_version(PIN), TAG)

    def test_already_padded_unchanged(self) -> None:
        self.assertEqual(normalize_ytdlp_version(TAG), TAG)

    def test_single_digit_month_and_day(self) -> None:
        self.assertEqual(normalize_ytdlp_version("2026.1.5"), "2026.01.05")

    def test_unsupported_forms_die(self) -> None:
        for raw in (
            "2026.8",
            "2026.8.19.1",
            "26.8.19",
            "2026.x.19",
            "2026.8.x",
            "2026.13.19",
            "2026.8.32",
            "2026.123.19",
        ):
            with self.subTest(raw=raw):
                with self.assertRaises(SystemExit):
                    normalize_ytdlp_version(raw)


class TestLoadYtdlpVersion(unittest.TestCase):
    def write_pin(self, text: str) -> Path:
        f = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
        f.write(text)
        f.close()
        self.addCleanup(Path(f.name).unlink)
        return Path(f.name)

    def test_parses_pin_ignoring_comments(self) -> None:
        path = self.write_pin("# header comment\n\nyt-dlp==2026.8.19\n# trailer\n")
        self.assertEqual(load_ytdlp_version(path), TAG)

    def test_tolerates_whitespace_around_operator(self) -> None:
        path = self.write_pin("yt-dlp == 2026.8.19\n")
        self.assertEqual(load_ytdlp_version(path), TAG)

    def test_tolerates_inline_comment(self) -> None:
        path = self.write_pin("yt-dlp==2026.8.19  # bumped by dependabot\n")
        self.assertEqual(load_ytdlp_version(path), TAG)

    def test_missing_pin_dies(self) -> None:
        path = self.write_pin("# ffmpeg is pinned in fetch_sidecars.py\n")
        with self.assertRaises(SystemExit):
            load_ytdlp_version(path)

    def test_duplicate_pins_die(self) -> None:
        path = self.write_pin("yt-dlp==2026.8.19\nyt-dlp==2026.7.9\n")
        with self.assertRaises(SystemExit):
            load_ytdlp_version(path)

    def test_repo_pin_file_parses(self) -> None:
        repo_file = Path(__file__).resolve().parent / "requirements-sidecars.txt"
        self.assertRegex(load_ytdlp_version(repo_file), r"^\d{4}\.\d{2}\.\d{2}$")


class TestFfmpegBranch(unittest.TestCase):
    def test_derives_major_minor(self) -> None:
        self.assertEqual(ffmpeg_branch("9.0.1"), "9.0")

    def test_unsupported_shapes_die(self) -> None:
        for raw in ("9", "9.0", "9.0.1.1", "9.0.x", "x.y.z"):
            with self.subTest(raw=raw):
                with self.assertRaises(SystemExit):
                    ffmpeg_branch(raw)


class TestFfmpegUrl(unittest.TestCase):
    """File names assert the full BtbN naming contract; the version segment
    derives from FFMPEG_VERSION so bumping the pin does not break tests."""

    def branch(self) -> str:
        return ffmpeg_branch(FFMPEG_VERSION)

    def test_darwin_none(self) -> None:
        self.assertIsNone(ffmpeg_download_url("Darwin", "arm64"))

    def test_linux_x64(self) -> None:
        u = ffmpeg_download_url("Linux", "x86_64")
        assert u is not None
        self.assertTrue(
            u.endswith(f"/ffmpeg-n{self.branch()}-latest-linux64-gpl-{self.branch()}.tar.xz")
        )

    def test_linux_arm64(self) -> None:
        u = ffmpeg_download_url("Linux", "aarch64")
        assert u is not None
        self.assertTrue(
            u.endswith(f"/ffmpeg-n{self.branch()}-latest-linuxarm64-gpl-{self.branch()}.tar.xz")
        )

    def test_windows_x64(self) -> None:
        u = ffmpeg_download_url("Windows", "x86_64")
        assert u is not None
        self.assertTrue(
            u.endswith(f"/ffmpeg-n{self.branch()}-latest-win64-gpl-{self.branch()}.zip")
        )

    def test_windows_arm64(self) -> None:
        u = ffmpeg_download_url("Windows", "arm64")
        assert u is not None
        self.assertTrue(
            u.endswith(f"/ffmpeg-n{self.branch()}-latest-winarm64-gpl-{self.branch()}.zip")
        )

    def test_ffmpeg_version_shape(self) -> None:
        self.assertRegex(FFMPEG_VERSION, r"^\d+\.\d+\.\d+$")


if __name__ == "__main__":
    unittest.main()
