#!/usr/bin/env python3
from __future__ import annotations

import os
import unittest
from unittest import mock

import check_sidecar_upstream as chk

# A trimmed copy of martin-riedl's index page. The real one links one release
# build and one nightly per target, and the split between them is the whole
# reason the checker filters: the nightly is always newer.
INDEX_PAGE = """
<a href="/download/macos/arm64/1789407207_N-126556-g639ee84952/ffmpeg.zip">nightly</a>
<a href="/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip">release</a>
<a href="/download/macos/amd64/1767299902_N-122320-g38e89fe502/ffmpeg.zip">intel</a>
<a href="/download/linux/amd64/1789931100_9.0.2/ffmpeg.tar.xz">linux arm64?</a>
"""

# Real asset names from an autobuild snapshot: our variant, the same variant
# under a different license, the branchless master build, and an older branch.
BTBN_ASSETS = [
    "ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0.tar.xz",
    "ffmpeg-n9.0.2-3-ga5923073bf-linuxarm64-gpl-9.0.tar.xz",
    "ffmpeg-n9.0.2-3-ga5923073bf-win64-gpl-9.0.zip",
    "ffmpeg-n9.0.2-3-ga5923073bf-winarm64-gpl-9.0.zip",
    "ffmpeg-n9.0.2-3-ga5923073bf-linux64-lgpl-9.0.tar.xz",
    "ffmpeg-N-126734-ga9cbcc2bbb-linux64-gpl.tar.xz",
    "ffmpeg-n8.1.3-linux64-gpl-8.1.tar.xz",
    "checksums.sha256",
]

PINNED_LINUX64 = "ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0.tar.xz"


class TestVersionKeys(unittest.TestCase):
    def test_version_key_is_numeric(self) -> None:
        self.assertEqual(chk.version_key("9.0.2"), (9, 0, 2))
        self.assertGreater(chk.version_key("10.0"), chk.version_key("9.0.2"))

    def test_version_key_rejects_nightly_versions(self) -> None:
        with self.assertRaises(ValueError):
            chk.version_key("N-126556-g639ee84952")

    def test_build_id_key_sorts_version_before_timestamp(self) -> None:
        older_version_newer_stamp = "2000000000_9.0.2"
        newer_version_older_stamp = "1000000000_9.0.3"
        self.assertGreater(
            chk.build_id_key(newer_version_older_stamp),
            chk.build_id_key(older_version_newer_stamp),
        )

    def test_build_timestamp_orders_a_mixed_listing(self) -> None:
        self.assertEqual(chk.build_timestamp("1789407207_N-1-gab"), 1789407207)
        with self.assertRaises(ValueError):
            chk.build_timestamp("9.0.2")

    def test_build_id_key_breaks_ties_on_the_timestamp(self) -> None:
        self.assertGreater(
            chk.build_id_key("1789931890_9.0.2"),
            chk.build_id_key("1789407207_9.0.2"),
        )


class TestIndexPage(unittest.TestCase):
    def test_lists_only_the_requested_target(self) -> None:
        self.assertEqual(
            chk.listed_build_ids(INDEX_PAGE, "macos/arm64"),
            ["1789407207_N-126556-g639ee84952", "1789931890_9.0.2"],
        )

    def test_newest_release_build_skips_the_nightly(self) -> None:
        # The nightly carries the newer-looking id on purpose: taking the
        # maximum build id outright would report it every month.
        self.assertEqual(
            chk.newest_release_build(INDEX_PAGE, "macos/arm64"),
            "1789931890_9.0.2",
        )

    def test_newest_release_build_returns_none_without_a_release(self) -> None:
        nightly_only = '<a href="/download/macos/arm64/1789407207_N-1-gabc/ffmpeg.zip">n</a>'
        self.assertIsNone(chk.newest_release_build(nightly_only, "macos/arm64"))


class TestBtbnAssets(unittest.TestCase):
    def test_variant_keeps_the_tail_and_drops_the_token(self) -> None:
        self.assertEqual(chk.btbn_variant(PINNED_LINUX64), "-linux64-gpl-9.0.tar.xz")
        self.assertEqual(chk.btbn_token(PINNED_LINUX64), "n9.0.2-3-ga5923073bf")

    def test_variant_rejects_an_unknown_name(self) -> None:
        with self.assertRaises(ValueError):
            chk.btbn_variant("checksums.sha256")

    def test_a_current_variant_reports_nothing(self) -> None:
        self.assertIsNone(chk.btbn_finding(BTBN_ASSETS, PINNED_LINUX64))

    def test_a_moved_token_is_reported(self) -> None:
        moved = [
            name.replace("n9.0.2-3-ga5923073bf", "n9.0.2-5-gdeadbeef00")
            for name in BTBN_ASSETS
        ]
        self.assertEqual(
            chk.btbn_finding(moved, PINNED_LINUX64),
            "ffmpeg (Linux/Windows) n9.0.2-3-ga5923073bf → n9.0.2-5-gdeadbeef00",
        )

    def test_a_vanished_variant_is_reported(self) -> None:
        finding = chk.btbn_finding(
            [n for n in BTBN_ASSETS if "gpl-9.0" not in n], PINNED_LINUX64
        )
        assert finding is not None
        self.assertIn("-linux64-gpl-9.0.tar.xz", finding)

    def test_another_license_is_not_mistaken_for_ours(self) -> None:
        # A -linux64-lgpl-9.0 asset must never satisfy a -linux64-gpl-9.0 pin.
        lgpl_only = [n for n in BTBN_ASSETS if "lgpl" in n]
        self.assertIsNotNone(chk.btbn_finding(lgpl_only, PINNED_LINUX64))


class TestNewestAutobuild(unittest.TestCase):
    def test_skips_the_mutable_latest_tag(self) -> None:
        releases = [
            {"tag_name": "latest", "assets": []},
            {"tag_name": "autobuild-2026-09-21-13-55", "assets": []},
            {"tag_name": "autobuild-2026-09-20-13-11", "assets": []},
        ]
        self.assertEqual(
            chk.newest_autobuild(releases)["tag_name"], "autobuild-2026-09-21-13-55"
        )

    def test_dies_without_an_autobuild(self) -> None:
        with self.assertRaises(ValueError):
            chk.newest_autobuild([{"tag_name": "latest", "assets": []}])


class TestGithubHeaders(unittest.TestCase):
    def test_uses_a_token_when_the_environment_has_one(self) -> None:
        with mock.patch.dict(os.environ, {"GITHUB_TOKEN": "t0ken"}):
            self.assertEqual(chk.github_headers()["Authorization"], "Bearer t0ken")

    def test_omits_the_token_when_there_is_none(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertNotIn("Authorization", chk.github_headers())


if __name__ == "__main__":
    unittest.main()
