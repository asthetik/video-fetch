#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import unittest
from unittest import mock

import check_sidecar_upstream as chk
import fetch_sidecars as fs

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


class TestCollectFindingsNetworkLayer(unittest.TestCase):
    """collect_findings driven end-to-end over a fake fetch_bytes: the real
    URL set, the real headers, and the real finding logic — everything above
    the socket."""

    def setUp(self) -> None:
        pins = mock.patch.object(
            chk,
            "load_pins",
            return_value={
                "downloads": [
                    PINNED_LINUX64,
                    "ffmpeg-n9.0.2-3-ga5923073bf-win64-gpl-9.0.zip",
                ]
            },
        )
        ytdlp = mock.patch.object(chk, "load_ytdlp_version", return_value="2026.11.23")
        macos = mock.patch.object(chk, "FFMPEG_MACOS_BUILD", "1789931890_9.0.2")
        pins.start()
        ytdlp.start()
        macos.start()
        self.addCleanup(pins.stop)
        self.addCleanup(ytdlp.stop)
        self.addCleanup(macos.stop)

    @staticmethod
    def fake_fetch(responses: dict[str, bytes]):
        """A fetch_bytes stub keyed by URL substring, recording every call."""
        calls: list[tuple[str, dict[str, str] | None]] = []

        def fetch(url: str, headers: dict[str, str] | None = None) -> bytes:
            calls.append((url, headers))
            for key, body in responses.items():
                if key in url:
                    return body
            raise AssertionError(f"unexpected fetch: {url}")

        return fetch, calls

    @staticmethod
    def btbody(assets: list[str]) -> bytes:
        # The API returns a release LIST, newest first; newest_autobuild
        # picks the first autobuild- tagged entry.
        return json.dumps(
            [
                {
                    "tag_name": "autobuild-2026-11-24-12-41",
                    "assets": [{"name": n} for n in assets],
                }
            ]
        ).encode()

    def test_all_current_lists_no_findings(self) -> None:
        yt = json.dumps({"tag_name": "2026.11.23"}).encode()
        fake, calls = self.fake_fetch(
            {
                "releases/latest": yt,
                "releases?per_page": self.btbody(BTBN_ASSETS),
                "martin-riedl": INDEX_PAGE.encode(),
            }
        )
        with mock.patch.object(chk, "fetch_bytes", fake):
            self.assertEqual(chk.collect_findings(), [])
        self.assertEqual(len(calls), 3)
        # The GitHub API calls carry the headers; the plain index page does not.
        self.assertIsNotNone(calls[0][1])
        self.assertIn("Accept", calls[0][1])
        self.assertIsNone(calls[2][1])

    def test_every_outdated_surface_reports(self) -> None:
        yt = json.dumps({"tag_name": "2026.12.01"}).encode()
        moved = [
            n.replace("n9.0.2-3-ga5923073bf", "n9.1.0-1-gdeadbeef")
            for n in BTBN_ASSETS[:4]
        ] + BTBN_ASSETS[4:]
        newer_page = INDEX_PAGE.replace("1789931890_9.0.2", "1790000000_9.1.0")
        fake, _ = self.fake_fetch(
            {
                "releases/latest": yt,
                "releases?per_page": self.btbody(moved),
                "martin-riedl": newer_page.encode(),
            }
        )
        with mock.patch.object(chk, "fetch_bytes", fake):
            findings = chk.collect_findings()
        # yt-dlp, both pinned BtbN targets (linux64 + win64), and macOS.
        self.assertEqual(len(findings), 4, findings)
        self.assertIn("yt-dlp 2026.11.23 → 2026.12.01", findings)
        self.assertEqual(
            sum(1 for f in findings if "ffmpeg (Linux/Windows)" in f),
            2,
            "each pinned BtbN target reports its own difference",
        )
        self.assertTrue(any("ffmpeg (macOS)" in f for f in findings))

    def test_github_token_rides_the_api_headers(self) -> None:
        yt = json.dumps({"tag_name": "2026.11.23"}).encode()
        seen: dict[str, dict[str, str] | None] = {}

        def fetch(url: str, headers: dict[str, str] | None = None) -> bytes:
            seen[url] = headers
            for key, body in {
                "releases/latest": yt,
                "releases?per_page": self.btbody(BTBN_ASSETS),
                "martin-riedl": INDEX_PAGE.encode(),
            }.items():
                if key in url:
                    return body
            raise AssertionError(f"unexpected fetch: {url}")

        with mock.patch.dict(os.environ, {"GITHUB_TOKEN": "t0k3n"}), mock.patch.object(
            chk, "fetch_bytes", fetch
        ):
            chk.collect_findings()
        api_headers = next(h for u, h in seen.items() if "api.github.com" in u)
        self.assertEqual(api_headers.get("Authorization"), "Bearer t0k3n")


class TestFetchBytesLoopback(unittest.TestCase):
    """fetch_bytes against a real localhost server: the only layer that
    exercises the actual urllib path (headers, status handling) end to end."""

    @classmethod
    def setUpClass(cls) -> None:
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                if self.path == "/ok":
                    body = b"hello upstream"
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                elif self.path == "/auth":
                    body = (
                        b"ok"
                        if self.headers.get("Authorization") == "Bearer t0k3n"
                        else b"no"
                    )
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()

            def log_message(self, *args: object) -> None:
                pass

        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()

    def test_fetch_bytes_roundtrips_bytes(self) -> None:
        self.assertEqual(
            fs.fetch_bytes(f"{self.base}/ok"), b"hello upstream"
        )

    def test_fetch_bytes_carries_headers(self) -> None:
        body = fs.fetch_bytes(f"{self.base}/auth", {"Authorization": "Bearer t0k3n"})
        self.assertEqual(body, b"ok")

    def test_http_error_exits_instead_of_returning(self) -> None:
        # die() over a failed fetch must exit the job, not return garbage.
        with self.assertRaises(SystemExit):
            fs.fetch_bytes(f"{self.base}/missing")
