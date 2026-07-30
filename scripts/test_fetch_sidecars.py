#!/usr/bin/env python3
from __future__ import annotations

import unittest

from fetch_sidecars import ffmpeg_download_url, ytdlp_download_url


class TestYtdlpUrl(unittest.TestCase):
    def test_darwin(self) -> None:
        self.assertTrue(ytdlp_download_url("Darwin", "arm64").endswith("/yt-dlp_macos"))

    def test_linux_x64_standalone(self) -> None:
        self.assertTrue(ytdlp_download_url("Linux", "x86_64").endswith("/yt-dlp_linux"))

    def test_linux_arm64(self) -> None:
        self.assertTrue(
            ytdlp_download_url("Linux", "aarch64").endswith("/yt-dlp_linux_aarch64")
        )

    def test_windows_x64(self) -> None:
        self.assertTrue(ytdlp_download_url("Windows", "AMD64").endswith("/yt-dlp.exe"))

    def test_windows_arm64(self) -> None:
        self.assertTrue(
            ytdlp_download_url("Windows", "ARM64").endswith("/yt-dlp_arm64.exe")
        )


class TestFfmpegUrl(unittest.TestCase):
    def test_darwin_none(self) -> None:
        self.assertIsNone(ffmpeg_download_url("Darwin", "arm64"))

    def test_linux_x64(self) -> None:
        u = ffmpeg_download_url("Linux", "x86_64")
        assert u is not None
        self.assertIn("linux64-gpl.tar.xz", u)

    def test_linux_arm64(self) -> None:
        u = ffmpeg_download_url("Linux", "aarch64")
        assert u is not None
        self.assertIn("linuxarm64-gpl.tar.xz", u)

    def test_windows_arm64(self) -> None:
        u = ffmpeg_download_url("Windows", "arm64")
        assert u is not None
        self.assertIn("winarm64-gpl.zip", u)

    def test_windows_x64(self) -> None:
        u = ffmpeg_download_url("Windows", "x86_64")
        assert u is not None
        self.assertIn("win64-gpl.zip", u)


if __name__ == "__main__":
    unittest.main()
