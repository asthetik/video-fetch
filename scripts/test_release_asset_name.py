#!/usr/bin/env python3
"""Unit tests for release installer filenames (cc-switch style)."""

from __future__ import annotations

import unittest

from release_asset_name import asset_filename


class TestAssetFilename(unittest.TestCase):
    def test_macos_omits_arch(self) -> None:
        self.assertEqual(
            asset_filename("macOS", "arm64", "0.1.2", "dmg"),
            "Video-Fetch-v0.1.2-macOS.dmg",
        )

    def test_windows_x64_includes_arch(self) -> None:
        self.assertEqual(
            asset_filename("Windows", "x64", "0.1.2", "msi"),
            "Video-Fetch-v0.1.2-Windows-x64.msi",
        )
        self.assertEqual(
            asset_filename("Windows", "x64", "0.1.2", "exe"),
            "Video-Fetch-v0.1.2-Windows-x64.exe",
        )

    def test_windows_arm64_includes_arch(self) -> None:
        self.assertEqual(
            asset_filename("Windows", "arm64", "0.1.2", "msi"),
            "Video-Fetch-v0.1.2-Windows-arm64.msi",
        )
        self.assertEqual(
            asset_filename("Windows", "arm64", "0.1.2", "exe"),
            "Video-Fetch-v0.1.2-Windows-arm64.exe",
        )

    def test_linux_always_includes_arch(self) -> None:
        self.assertEqual(
            asset_filename("Linux", "x86_64", "0.1.2", "AppImage"),
            "Video-Fetch-v0.1.2-Linux-x86_64.AppImage",
        )
        self.assertEqual(
            asset_filename("Linux", "x86_64", "0.1.2", "deb"),
            "Video-Fetch-v0.1.2-Linux-x86_64.deb",
        )
        self.assertEqual(
            asset_filename("Linux", "arm64", "0.1.2", "AppImage"),
            "Video-Fetch-v0.1.2-Linux-arm64.AppImage",
        )
        self.assertEqual(
            asset_filename("Linux", "arm64", "0.1.2", "deb"),
            "Video-Fetch-v0.1.2-Linux-arm64.deb",
        )

    def test_version_strips_leading_v(self) -> None:
        self.assertEqual(
            asset_filename("macOS", "arm64", "v0.1.2", "dmg"),
            "Video-Fetch-v0.1.2-macOS.dmg",
        )

    def test_rejects_unknown_os(self) -> None:
        with self.assertRaises(ValueError):
            asset_filename("Darwin", "arm64", "0.1.2", "dmg")


if __name__ == "__main__":
    unittest.main()
