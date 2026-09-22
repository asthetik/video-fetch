#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import zipfile
from collections.abc import Sequence
from pathlib import Path
from unittest import mock

import fetch_sidecars
from fetch_sidecars import (
    BTBN_ARCHES,
    FFMPEG_BTBN_TAG,
    FFMPEG_MACOS_BUILD,
    FFMPEG_MACOS_SIGNER,
    FFMPEG_MACOS_TEAM_ID,
    FFMPEG_VERSION,
    PinsRecorder,
    elf_machine,
    extract_tar_xz,
    fetch_ffmpeg,
    fetch_ytdlp,
    ffmpeg_branch,
    ffmpeg_download_url,
    ffmpeg_macos_artifact,
    ffmpeg_macos_url,
    ffmpeg_snapshot,
    load_pins,
    load_ytdlp_version,
    macho_cpu_types,
    normalize_ytdlp_version,
    parse_shasums,
    pe_machine,
    pin_key_from_url,
    sha256_hex,
    verify_btbn_ffmpeg_arch,
    verify_ffmpeg_snapshot,
    verify_macos_ffmpeg,
    verify_sha256,
    write_pins,
    ytdlp_download_url,
    ytdlp_pin_key,
)

PIN = "2026.8.19"
TAG = "2026.08.19"

# Known-answer vector for simple digest tests.
BLOB = b"abc"
BLOB_SHA256 = hashlib.sha256(BLOB).hexdigest()


def _write_pins(text: str) -> Path:
    f = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
    f.write(text)
    f.close()
    return Path(f.name)


def write_tar_xz(
    path: Path,
    files: Sequence[tuple[str, bytes]] = (),
    links: Sequence[tuple[str, str]] = (),
) -> None:
    with tarfile.open(path, "w:xz") as tf:
        for name, data in files:
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tf.addfile(info, io.BytesIO(data))
        for name, target in links:
            info = tarfile.TarInfo(name)
            info.type = tarfile.SYMTYPE
            info.linkname = target
            tf.addfile(info)


class TmpDirTestCase(unittest.TestCase):
    def tmpdir(self) -> Path:
        d = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, d, ignore_errors=True)
        return d


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


class TestYtdlpPinKeys(unittest.TestCase):
    def test_key_appends_the_release_tag(self) -> None:
        self.assertEqual(
            ytdlp_pin_key("yt-dlp_linux", "2026.08.19"), "yt-dlp_linux@2026.08.19"
        )

    def test_fetch_verifies_against_the_versioned_key(self) -> None:
        seen: list[str | None] = []

        def spy(
            url: str, dest: Path, pins: dict, artifact: str | None = None
        ) -> None:
            seen.append(artifact)
            Path(dest).write_bytes(b"stub")

        original = fetch_sidecars.download_and_verify
        fetch_sidecars.download_and_verify = spy
        self.addCleanup(
            lambda: setattr(fetch_sidecars, "download_and_verify", original)
        )

        with tempfile.TemporaryDirectory() as tmp_s:
            fetch_ytdlp(
                "Linux",
                "x86_64",
                Path(tmp_s) / "yt-dlp",
                "2026.08.19",
                {"downloads": {}},
            )

        self.assertEqual(seen, ["yt-dlp_linux@2026.08.19"])

    def test_fetch_dies_when_only_the_legacy_unversioned_key_is_pinned(self) -> None:
        # The versioned key is mandatory: the download path must not
        # silently fall back to the pre-@tag asset name.
        with tempfile.TemporaryDirectory() as tmp_s:
            with mock.patch.object(
                fetch_sidecars,
                "download",
                lambda url, dest: Path(dest).write_bytes(b"stub"),
            ):
                with self.assertRaises(SystemExit):
                    fetch_ytdlp(
                        "Linux",
                        "x86_64",
                        Path(tmp_s) / "yt-dlp",
                        TAG,
                        {"downloads": {"yt-dlp_linux": "a" * 64}},
                    )


class TestYtdlpCacheAnchor(TmpDirTestCase):
    """A warm cache must not outlive the release it was fetched for.

    `binaries.yt-dlp` is keyed by target triple, so bumping the version pin
    alone leaves a cache that still matches it. The versioned downloads pin
    is what ties the cache to the pinned release.
    """

    TRIPLE = "x86_64-unknown-linux-gnu"
    RELEASE_KEY = "yt-dlp_linux@2026.08.19"

    def pins(self, release: str | None, binary: str | None = BLOB_SHA256) -> dict:
        return {
            "downloads": {} if release is None else {self.RELEASE_KEY: release},
            "binaries": {} if binary is None else {"yt-dlp": {self.TRIPLE: binary}},
        }

    def cached(self, payload: bytes = BLOB) -> Path:
        path = self.tmpdir() / f"yt-dlp-{self.TRIPLE}"
        path.write_bytes(payload)
        return path

    def needs_fetch(self, path: Path, pins: dict) -> bool:
        return fetch_sidecars.ytdlp_needs_fetch(
            path, pins, self.TRIPLE, "Linux", "x86_64", TAG
        )

    def test_cache_matching_the_pinned_release_is_kept(self) -> None:
        self.assertFalse(self.needs_fetch(self.cached(), self.pins(BLOB_SHA256)))

    def test_cache_missing_its_release_pin_is_refetched(self) -> None:
        # requirements-sidecars.txt bumped and committed without
        # --update-pins: the new release has no key, while the cached binary
        # still matches the superseded release's binaries pin. Cold release
        # runners would fail loudly; a warm cache must not ship the old one.
        self.assertTrue(self.needs_fetch(self.cached(), self.pins(release=None)))

    def test_cache_pinned_to_another_release_is_refetched(self) -> None:
        self.assertTrue(self.needs_fetch(self.cached(), self.pins("9" * 64)))

    def test_cache_holding_other_bytes_is_refetched(self) -> None:
        self.assertTrue(
            self.needs_fetch(self.cached(b"substituted bytes"), self.pins(BLOB_SHA256))
        )

    def test_cache_without_a_binary_pin_is_refetched(self) -> None:
        self.assertTrue(
            self.needs_fetch(self.cached(), self.pins(BLOB_SHA256, binary=None))
        )

    def test_unsupported_platform_dies(self) -> None:
        with self.assertRaises(SystemExit):
            fetch_sidecars.ytdlp_needs_fetch(
                self.cached(), self.pins(BLOB_SHA256), self.TRIPLE, "Linux", "riscv64", TAG
            )


class TestPinsRecorderPolicy(unittest.TestCase):
    """What the versioned key buys: a bump is recorded, a rewrite is not."""

    def recorder(self, force: bool = False) -> PinsRecorder:
        return PinsRecorder({"downloads": {}, "binaries": {}}, force)

    def test_a_new_key_is_recorded_without_force(self) -> None:
        recorder = self.recorder()
        recorder.record("yt-dlp_linux@2026.09.20", "a" * 64)
        self.assertFalse(recorder.stale)
        self.assertEqual(
            recorder.pins["downloads"]["yt-dlp_linux@2026.09.20"], "a" * 64
        )

    def test_a_changed_digest_under_the_same_key_needs_force(self) -> None:
        recorder = self.recorder()
        recorder.record("yt-dlp_linux@2026.08.19", "a" * 64)
        recorder.record("yt-dlp_linux@2026.08.19", "b" * 64)
        self.assertTrue(recorder.stale)
        self.assertEqual(
            recorder.pins["downloads"]["yt-dlp_linux@2026.08.19"], "a" * 64
        )

    def test_force_repins_a_changed_digest(self) -> None:
        recorder = self.recorder(force=True)
        recorder.record("yt-dlp_linux@2026.08.19", "a" * 64)
        recorder.record("yt-dlp_linux@2026.08.19", "b" * 64)
        self.assertFalse(recorder.stale)
        self.assertEqual(
            recorder.pins["downloads"]["yt-dlp_linux@2026.08.19"], "b" * 64
        )


class TestUpdatePinsRecordsVersionedKeys(unittest.TestCase):
    """The wiring in update_pins: yt-dlp digests land under versioned keys,
    and keys from a superseded release (including pre-@tag legacy ones) are
    pruned by the same run. The run is stopped right after the yt-dlp pass
    (btbn_upstream_shasums exits), before any network download."""

    def test_records_versioned_keys_and_prunes_superseded_ones(self) -> None:
        pins = {
            "downloads": {
                "yt-dlp_linux@2026.01.01": "c" * 64,
                "yt-dlp_macos": "d" * 64,  # legacy pre-@tag key
            },
            # The triple-keyed binary digests must go with the release they
            # describe, or the re-record below lands as CHANGED/--force.
            "binaries": {"yt-dlp": {"x86_64-unknown-linux-gnu": "e" * 64}},
        }
        created: list[PinsRecorder] = []

        class Spy(PinsRecorder):
            def __init__(self, table: dict, force: bool) -> None:
                super().__init__(table, force)
                created.append(self)

        sums = {
            name: "a" * 64
            for name in (
                "yt-dlp_linux",
                "yt-dlp_linux_aarch64",
                "yt-dlp_macos",
                "yt-dlp.exe",
                "yt-dlp_arm64.exe",
            )
        }
        with (
            mock.patch.object(fetch_sidecars, "load_pins", return_value=pins),
            mock.patch.object(fetch_sidecars, "PinsRecorder", Spy),
            mock.patch.object(fetch_sidecars, "load_ytdlp_version", return_value=TAG),
            mock.patch.object(
                fetch_sidecars, "ytdlp_upstream_shasums", return_value=sums
            ),
            mock.patch.object(
                fetch_sidecars, "btbn_upstream_shasums", side_effect=SystemExit
            ),
        ):
            with self.assertRaises(SystemExit):
                fetch_sidecars.update_pins(force=False)

        recorded = created[0].pins["downloads"]
        for artifact in sums:
            self.assertIn(f"{artifact}@{TAG}", recorded)
        self.assertNotIn("yt-dlp_linux@2026.01.01", recorded)
        self.assertNotIn("yt-dlp_macos", recorded)
        self.assertFalse(set(sums) & set(recorded))
        self.assertNotIn("yt-dlp", created[0].pins["binaries"])


class TestUpdatePinsPrunesSupersededFfmpegAssets(unittest.TestCase):
    """A build bump rewrites the asset names the ffmpeg keys carry. Left
    behind, the superseded keys make btbn_asset_name ambiguous and turn the
    binary re-record into CHANGED/--force. The update prunes them and drops
    binaries.ffmpeg, the way the yt-dlp pass does. Runs are stopped at the
    macOS checksum fetch: after the prune, before any download."""

    def btbn_name(self, build: str, arch: str) -> str:
        ext = "zip" if arch.startswith("win") else "tar.xz"
        return f"ffmpeg-n9.0.2-{build}-{arch}-gpl-{ffmpeg_branch()}.{ext}"

    def btbn_sums(self, build: str = "9-gcafebabe") -> dict[str, str]:
        return {
            self.btbn_name(build, arch): "a" * 64
            for arch in ("linux64", "linuxarm64", "win64", "winarm64")
        }

    def ytdlp_sums(self) -> dict[str, str]:
        return {
            name: "b" * 64
            for name in (
                "yt-dlp_linux",
                "yt-dlp_linux_aarch64",
                "yt-dlp_macos",
                "yt-dlp.exe",
                "yt-dlp_arm64.exe",
            )
        }

    def run_update(
        self, pins: dict, btbn_sums: dict, ytdlp_sums: dict
    ) -> PinsRecorder:
        created: list[PinsRecorder] = []

        class Spy(PinsRecorder):
            def __init__(self, table: dict, force: bool) -> None:
                super().__init__(table, force)
                created.append(self)

        with (
            mock.patch.object(fetch_sidecars, "load_pins", return_value=pins),
            mock.patch.object(fetch_sidecars, "PinsRecorder", Spy),
            mock.patch.object(fetch_sidecars, "load_ytdlp_version", return_value=TAG),
            mock.patch.object(
                fetch_sidecars, "ytdlp_upstream_shasums", return_value=ytdlp_sums
            ),
            mock.patch.object(
                fetch_sidecars, "btbn_upstream_shasums", return_value=btbn_sums
            ),
            mock.patch.object(fetch_sidecars, "fetch_bytes", side_effect=SystemExit),
        ):
            with self.assertRaises(SystemExit):
                fetch_sidecars.update_pins(force=False)
        return created[0]

    def test_prunes_superseded_assets_and_drops_binaries(self) -> None:
        sums = self.btbn_sums()
        ytdlp_sums = self.ytdlp_sums()
        old_btbn = self.btbn_name("3-ga5923073bf", "linux64")
        old_macos = "ffmpeg-macos-arm64-1111_9.0.1.zip"
        ytdlp_key = ytdlp_pin_key("yt-dlp_linux", TAG)
        pins = {
            "downloads": {
                old_btbn: "c" * 64,
                old_macos: "d" * 64,
                ytdlp_key: ytdlp_sums["yt-dlp_linux"],
            },
            # The triple-keyed ffmpeg digests describe the superseded build:
            # re-recording the new one under those unchanged keys is the
            # CHANGED/--force routine the prune exists to prevent.
            "binaries": {
                "ffmpeg": {"x86_64-unknown-linux-gnu": "e" * 64},
                "yt-dlp": {"x86_64-unknown-linux-gnu": ytdlp_sums["yt-dlp_linux"]},
            },
        }
        recorder = self.run_update(pins, sums, ytdlp_sums)

        downloads = recorder.pins["downloads"]
        for name in sums:
            self.assertEqual(downloads.get(name), sums[name])
        self.assertNotIn(old_btbn, downloads)
        self.assertNotIn(old_macos, downloads)
        self.assertIn(ytdlp_key, downloads, "the yt-dlp pass must be untouched")
        self.assertNotIn("ffmpeg", recorder.pins["binaries"])
        self.assertIn("yt-dlp", recorder.pins["binaries"])
        self.assertFalse(recorder.stale, "a build bump must not need --force")

    def test_a_rebuilt_asset_under_an_unchanged_name_still_demands_force(
        self,
    ) -> None:
        # The prune must not launder a rebuild: bytes served under a name
        # this run still pins are CHANGED, and that stays an incident until
        # a human passes --force.
        sums = self.btbn_sums()
        rebuilt = self.btbn_name("9-gcafebabe", "linux64")
        pins = {"downloads": {rebuilt: "c" * 64}, "binaries": {}}
        recorder = self.run_update(pins, sums, self.ytdlp_sums())

        self.assertTrue(recorder.stale)
        self.assertEqual(recorder.pins["downloads"][rebuilt], "c" * 64)


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
    """File names assert the BtbN naming contract. The build identity
    (commits since tag + hash) is baked into every pinned asset name, so it
    is matched with a wildcard while the branch derives from FFMPEG_VERSION."""

    def branch(self) -> str:
        return ffmpeg_branch(FFMPEG_VERSION)

    def test_darwin_none(self) -> None:
        self.assertIsNone(ffmpeg_download_url("Darwin", "arm64"))

    def test_linux_x64(self) -> None:
        u = ffmpeg_download_url("Linux", "x86_64")
        assert u is not None
        self.assertRegex(
            u, rf"/ffmpeg-n{self.branch()}\..+-linux64-gpl-{self.branch()}\.tar\.xz$"
        )

    def test_linux_arm64(self) -> None:
        u = ffmpeg_download_url("Linux", "aarch64")
        assert u is not None
        self.assertRegex(
            u,
            rf"/ffmpeg-n{self.branch()}\..+-linuxarm64-gpl-{self.branch()}\.tar\.xz$",
        )

    def test_windows_x64(self) -> None:
        u = ffmpeg_download_url("Windows", "x86_64")
        assert u is not None
        self.assertRegex(
            u, rf"/ffmpeg-n{self.branch()}\..+-win64-gpl-{self.branch()}\.zip$"
        )

    def test_windows_arm64(self) -> None:
        u = ffmpeg_download_url("Windows", "arm64")
        assert u is not None
        self.assertRegex(
            u, rf"/ffmpeg-n{self.branch()}\..+-winarm64-gpl-{self.branch()}\.zip$"
        )

    def test_urls_use_the_pinned_snapshot_tag(self) -> None:
        for system, machine in (
            ("Linux", "x86_64"),
            ("Linux", "aarch64"),
            ("Windows", "x86_64"),
            ("Windows", "ARM64"),
        ):
            with self.subTest(platform=f"{system}/{machine}"):
                u = ffmpeg_download_url(system, machine)
                assert u is not None
                self.assertIn(f"/download/{FFMPEG_BTBN_TAG}/", u)
                self.assertNotIn("/download/latest/", u)

    def test_ffmpeg_version_shape(self) -> None:
        self.assertRegex(FFMPEG_VERSION, r"^\d+\.\d+\.\d+$")


class TestBtbnAssetResolution(unittest.TestCase):
    """--update-pins resolves each platform's asset against the run's
    in-memory table: the file the run has not written yet still holds the
    superseded build's names, so the recording loop would otherwise fetch
    the old build (or die on two matches under the same branch)."""

    def new_name(self, arch: str) -> str:
        ext = "zip" if arch.startswith("win") else "tar.xz"
        return f"ffmpeg-n9.0.2-9-gcafebabe-{arch}-gpl-{ffmpeg_branch()}.{ext}"

    def no_disk_reads(self):
        return mock.patch.object(
            fetch_sidecars,
            "load_pins",
            side_effect=AssertionError("read the pins file"),
        )

    def test_asset_name_comes_from_the_passed_downloads(self) -> None:
        downloads = {self.new_name("linux64"): "a" * 64}
        with self.no_disk_reads():
            self.assertEqual(
                fetch_sidecars.btbn_asset_name("Linux", "x86_64", downloads),
                self.new_name("linux64"),
            )

    def test_url_comes_from_the_passed_downloads(self) -> None:
        downloads = {self.new_name("win64"): "a" * 64}
        with self.no_disk_reads():
            url = fetch_sidecars.ffmpeg_download_url("Windows", "x86_64", downloads)
        assert url is not None
        self.assertTrue(url.endswith(self.new_name("win64")), url)


class TestFetchFfmpegUpdateResolution(TmpDirTestCase):
    """The --update-pins recording loop must resolve the URL from the
    recorder's table — this run's pruned superseded keys and newly recorded
    names — not from the on-disk file, which is written only at the end."""

    def test_url_is_resolved_from_the_recorder_table(self) -> None:
        name = f"ffmpeg-n9.0.2-9-gcafebabe-linux64-gpl-{ffmpeg_branch()}.tar.xz"
        recorder = PinsRecorder({"downloads": {name: "a" * 64}, "binaries": {}}, False)
        seen: list[str] = []

        def fake(url, dest, pins, artifact=None):
            seen.append(url)
            raise SystemExit

        with (
            mock.patch.object(fetch_sidecars, "download_and_verify", fake),
            mock.patch.object(
                fetch_sidecars,
                "load_pins",
                side_effect=AssertionError("read the pins file"),
            ),
        ):
            with self.assertRaises(SystemExit):
                fetch_ffmpeg("Linux", "x86_64", self.tmpdir() / "ffmpeg", recorder)
        self.assertTrue(seen, "download_and_verify was never reached")
        self.assertTrue(seen[0].endswith(name), seen)


class TestExtractTarXz(TmpDirTestCase):
    def write_archive(self, name: str, data: bytes = b"binary\n") -> Path:
        archive = self.tmpdir() / "ffmpeg.tar.xz"
        write_tar_xz(archive, files=[(name, data)])
        return archive

    def test_extracts_nested_binary(self) -> None:
        archive = self.write_archive("ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/bin/ffmpeg")
        dest = self.tmpdir()
        extract_tar_xz(archive, dest)
        out = dest / "ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0" / "bin" / "ffmpeg"
        self.assertEqual(out.read_bytes(), b"binary\n")

    def test_rejects_path_traversal(self) -> None:
        archive = self.write_archive("../escape.txt")
        # Nested dest so that ".." resolves inside this test's private base,
        # not the shared system temp dir.
        base = self.tmpdir()
        dest = base / "dest"
        dest.mkdir()
        with self.assertRaises(tarfile.TarError):
            extract_tar_xz(archive, dest)
        self.assertFalse((base / "escape.txt").exists())

    def test_passes_data_filter_explicitly(self) -> None:
        # 3.14 already defaults to "data", so the behavior tests above stay
        # green even if the kwarg is dropped; this pins it explicitly.
        archive = self.write_archive("pkg/bin/ffmpeg")
        dest = self.tmpdir()
        with mock.patch.object(tarfile, "open") as opener:
            extract_tar_xz(archive, dest)
        opener.return_value.__enter__.return_value.extractall.assert_called_once_with(
            dest, filter="data"
        )

    def test_rejects_escaping_symlink(self) -> None:
        archive = self.tmpdir() / "ffmpeg.tar.xz"
        member = "ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/bin/ffmpeg"
        write_tar_xz(archive, links=[(member, "../../../outside")])
        dest = self.tmpdir()
        with self.assertRaises(tarfile.TarError):
            extract_tar_xz(archive, dest)

    def test_corrupt_archive_raises(self) -> None:
        d = self.tmpdir()
        archive = d / "ffmpeg.tar.xz"
        archive.write_bytes(b"not an xz archive")
        with self.assertRaises(tarfile.TarError):
            extract_tar_xz(archive, self.tmpdir())


class TestFetchFfmpegLinux(TmpDirTestCase):
    def install_fake_download(self, members: list[tuple[str, bytes]]) -> None:
        def fake_download(
            url: str, dest: Path, pins: dict, artifact: str | None = None
        ) -> None:
            write_tar_xz(dest, files=members)

        original = fetch_sidecars.download_and_verify
        fetch_sidecars.download_and_verify = fake_download
        self.addCleanup(setattr, fetch_sidecars, "download_and_verify", original)

    def test_prefers_bin_ffmpeg_from_archive(self) -> None:
        out = self.tmpdir() / "ffmpeg-x86_64-unknown-linux-gnu"
        self.install_fake_download(
            [
                (
                    "ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/bin/ffmpeg",
                    elf_header(0x3E),
                ),
                ("ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/share/ffmpeg", b"decoy\n"),
            ]
        )
        fetch_ffmpeg("Linux", "x86_64", out, load_pins())
        self.assertEqual(out.read_bytes(), elf_header(0x3E))

    def test_archive_without_bin_ffmpeg_dies(self) -> None:
        out = self.tmpdir() / "ffmpeg-x86_64-unknown-linux-gnu"
        self.install_fake_download(
            [("ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/share/ffmpeg", b"decoy\n")]
        )
        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Linux", "x86_64", out, load_pins())

    def test_x64_payload_under_the_arm64_asset_dies(self) -> None:
        out = self.tmpdir() / "ffmpeg-aarch64-unknown-linux-gnu"
        self.install_fake_download(
            [
                (
                    "ffmpeg-n9.0.2-3-ga5923073bf-linuxarm64-gpl-9.0/bin/ffmpeg",
                    elf_header(0x3E),
                )
            ]
        )
        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Linux", "aarch64", out, load_pins())

    def test_x64_payload_under_the_arm64_asset_is_removed(self) -> None:
        out = self.tmpdir() / "ffmpeg-aarch64-unknown-linux-gnu"
        self.install_fake_download(
            [
                (
                    "ffmpeg-n9.0.2-3-ga5923073bf-linuxarm64-gpl-9.0/bin/ffmpeg",
                    elf_header(0x3E),
                )
            ]
        )
        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Linux", "aarch64", out, load_pins())
        self.assertFalse(
            out.exists(), "rejected bytes must not stay at the sidecar path"
        )


class TestFetchFfmpegWindows(TmpDirTestCase):
    def install_fake_download(self, members: list[tuple[str, bytes]]) -> None:
        def fake_download(
            url: str, dest: Path, pins: dict, artifact: str | None = None
        ) -> None:
            buf = io.BytesIO()
            with zipfile.ZipFile(buf, "w") as zf:
                for name, data in members:
                    zf.writestr(name, data)
            dest.write_bytes(buf.getvalue())

        original = fetch_sidecars.download_and_verify
        fetch_sidecars.download_and_verify = fake_download
        self.addCleanup(setattr, fetch_sidecars, "download_and_verify", original)

    def test_extracts_ffmpeg_exe_and_checks_its_arch(self) -> None:
        out = self.tmpdir() / "ffmpeg-x86_64-pc-windows-msvc.exe"
        self.install_fake_download(
            [
                (
                    "ffmpeg-n9.0.2-3-ga5923073bf-win64-gpl-9.0/bin/ffmpeg.exe",
                    pe_header(0x8664),
                ),
                (
                    "ffmpeg-n9.0.2-3-ga5923073bf-win64-gpl-9.0/bin/ffprobe.exe",
                    pe_header(0x8664),
                ),
            ]
        )
        fetch_ffmpeg("Windows", "AMD64", out, load_pins())
        self.assertEqual(out.read_bytes(), pe_header(0x8664))

    def test_x64_payload_under_the_arm64_asset_dies(self) -> None:
        out = self.tmpdir() / "ffmpeg-aarch64-pc-windows-msvc.exe"
        self.install_fake_download(
            [
                (
                    "ffmpeg-n9.0.2-3-ga5923073bf-winarm64-gpl-9.0/bin/ffmpeg.exe",
                    pe_header(0x8664),
                )
            ]
        )
        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Windows", "ARM64", out, load_pins())

    def test_x64_payload_under_the_arm64_asset_is_removed(self) -> None:
        out = self.tmpdir() / "ffmpeg-aarch64-pc-windows-msvc.exe"
        self.install_fake_download(
            [
                (
                    "ffmpeg-n9.0.2-3-ga5923073bf-winarm64-gpl-9.0/bin/ffmpeg.exe",
                    pe_header(0x8664),
                )
            ]
        )
        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Windows", "ARM64", out, load_pins())
        self.assertFalse(
            out.exists(), "rejected bytes must not stay at the sidecar path"
        )


class TestSha256Hex(unittest.TestCase):
    def test_known_vector(self) -> None:
        self.assertEqual(sha256_hex(BLOB), BLOB_SHA256)

    def test_empty(self) -> None:
        self.assertEqual(sha256_hex(b""), hashlib.sha256(b"").hexdigest())

    def test_different_payloads_differ(self) -> None:
        self.assertNotEqual(sha256_hex(b"abc"), sha256_hex(b"abd"))


class TestVerifySha256(unittest.TestCase):
    def test_match_returns_none(self) -> None:
        self.assertIsNone(verify_sha256("artifact.bin", BLOB_SHA256, BLOB_SHA256))

    def test_match_is_case_insensitive(self) -> None:
        self.assertIsNone(
            verify_sha256("artifact.bin", BLOB_SHA256.upper(), BLOB_SHA256)
        )

    def test_mismatch_dies(self) -> None:
        with self.assertRaises(SystemExit):
            verify_sha256("artifact.bin", "0" * 64, BLOB_SHA256)

    def test_mismatch_reports_both_digests(self) -> None:
        bad = "1" * 64
        with self.assertRaises(SystemExit):
            verify_sha256("artifact.bin", bad, BLOB_SHA256)


class TestParseShasums(unittest.TestCase):
    def test_parses_standard_lines(self) -> None:
        text = (
            f"{'a' * 64}  yt-dlp_linux\n"
            f"{'b' * 64}  yt-dlp_macos\n"
        )
        self.assertEqual(
            parse_shasums(text),
            {"yt-dlp_linux": "a" * 64, "yt-dlp_macos": "b" * 64},
        )

    def test_accepts_binary_mode_marker(self) -> None:
        self.assertEqual(
            parse_shasums(f"{'c' * 64} *ffmpeg-n9.0.2-3-ga5923073bf-win64-gpl-9.0.zip\n"),
            {"ffmpeg-n9.0.2-3-ga5923073bf-win64-gpl-9.0.zip": "c" * 64},
        )

    def test_ignores_blank_lines_and_comments(self) -> None:
        self.assertEqual(
            parse_shasums(
                f"# header\n\n{'d' * 64}  ffmpeg-macos-arm64-{FFMPEG_MACOS_BUILD}.zip\n"
            ),
            {f"ffmpeg-macos-arm64-{FFMPEG_MACOS_BUILD}.zip": "d" * 64},
        )

    def test_keeps_filenames_with_spaces(self) -> None:
        self.assertEqual(
            parse_shasums(f"{'e' * 64}  ffmpeg macos.zip\n"),
            {"ffmpeg macos.zip": "e" * 64},
        )

    def test_ignores_shapes_that_cannot_identify_an_asset(self) -> None:
        self.assertEqual(parse_shasums("garbage\n" + "f" * 63 + "  short.txt\n"), {})

    def test_uppercase_digest_is_normalized(self) -> None:
        self.assertEqual(
            parse_shasums(f"{'A' * 64}  thing.bin\n"),
            {"thing.bin": "a" * 64},
        )


class TestPinKeyFromUrl(unittest.TestCase):
    def test_ytdlp_uses_release_asset_basename(self) -> None:
        self.assertEqual(
            pin_key_from_url(ytdlp_download_url("Linux", "x86_64", TAG)),
            "yt-dlp_linux",
        )
        self.assertEqual(
            pin_key_from_url(ytdlp_download_url("Darwin", "arm64", TAG)),
            "yt-dlp_macos",
        )

    def test_ffmpeg_btbn_uses_pinned_asset_basename(self) -> None:
        u = ffmpeg_download_url("Linux", "x86_64")
        assert u is not None
        self.assertRegex(
            pin_key_from_url(u),
            r"^ffmpeg-n9\.0\.\S+-(linux64|linuxarm64|win64|winarm64)-gpl-9\.0\.(tar\.xz|zip)$",
        )
        self.assertIn(FFMPEG_BTBN_TAG, u)

    def test_rejects_urls_without_basename(self) -> None:
        for url in ("https://example.com", "https://example.com/", ""):
            with self.subTest(url=url):
                with self.assertRaises(ValueError):
                    pin_key_from_url(url)


class TestLoadPins(unittest.TestCase):
    def test_reads_repo_pins_file(self) -> None:
        repo_file = Path(__file__).resolve().parent / "sidecar_pins.json"
        self.assertTrue(repo_file.is_file(), "sidecar_pins.json must be committed")
        pins = load_pins(repo_file)
        downloads = pins["downloads"]
        self.assertTrue(downloads, "pins file must pin at least one download")
        for name, digest in downloads.items():
            with self.subTest(name=name):
                self.assertRegex(digest, r"^[0-9a-f]{64}$")
                if name.startswith("yt-dlp"):
                    # yt-dlp's asset names are version-independent, so its
                    # pin key must append the release tag.
                    self.assertRegex(
                        name,
                        r"^(yt-dlp(_macos|_linux|_linux_aarch64|_arm64)?\.exe"
                        r"|yt-dlp_(macos|linux|linux_aarch64))"
                        r"@\d{4}\.\d{2}\.\d{2}$",
                        "unexpected yt-dlp pin key",
                    )
                else:
                    # ffmpeg's names already embed the build, so theirs
                    # must not.
                    self.assertRegex(
                        name,
                        r"^(ffmpeg-.*-(linux64|linuxarm64|win64|winarm64)-gpl-.*"
                        r"|ffmpeg-macos-arm64-\S+\.zip)$",
                        "unexpected ffmpeg pin key",
                    )

    def test_missing_file_dies(self) -> None:
        with self.assertRaises(SystemExit):
            load_pins(Path("/nonexistent/does-not-exist.json"))

    def test_malformed_json_dies(self) -> None:
        path = _write_pins("{not json")
        self.addCleanup(path.unlink)
        with self.assertRaises(SystemExit):
            load_pins(path)

    def test_wrong_shape_dies(self) -> None:
        for text in ('[]', '{"downloads": []}', '{"schema": 9, "downloads": {"a": "b"}}'):
            with self.subTest(text=text):
                path = _write_pins(text)
                self.addCleanup(path.unlink)
                with self.assertRaises(SystemExit):
                    load_pins(path)

    def test_bad_digest_in_file_dies(self) -> None:
        text = json.dumps({"downloads": {"yt-dlp_linux": "not-a-digest"}})
        path = _write_pins(text)
        self.addCleanup(path.unlink)
        with self.assertRaises(SystemExit):
            load_pins(path)

    def test_accepts_binary_section(self) -> None:
        text = json.dumps(
            {
                "downloads": {"yt-dlp_linux": "a" * 64},
                "binaries": {"yt-dlp": {"x86_64-unknown-linux-gnu": "b" * 64}},
            }
        )
        path = _write_pins(text)
        self.addCleanup(path.unlink)
        pins = load_pins(path)
        self.assertEqual(
            pins["binaries"]["yt-dlp"]["x86_64-unknown-linux-gnu"], "b" * 64
        )

    def test_binary_section_wrong_shape_dies(self) -> None:
        for binaries in ([], {"yt-dlp": "deadbeef"}, {"yt-dlp": {"triple": "short"}}):
            with self.subTest(binaries=binaries):
                path = _write_pins(
                    json.dumps({"downloads": {}, "binaries": binaries})
                )
                self.addCleanup(path.unlink)
                with self.assertRaises(SystemExit):
                    load_pins(path)

    def test_schema_2_requires_the_ffmpeg_snapshot_anchor(self) -> None:
        path = _write_pins(json.dumps({"schema": 2, "downloads": {}}))
        self.addCleanup(path.unlink)
        with self.assertRaises(SystemExit):
            load_pins(path)

    def test_schema_2_with_a_wellformed_anchor_loads(self) -> None:
        text = json.dumps(
            {"schema": 2, "downloads": {}, "ffmpeg_snapshot": ffmpeg_snapshot()}
        )
        path = _write_pins(text)
        self.addCleanup(path.unlink)
        self.assertEqual(load_pins(path)["ffmpeg_snapshot"], ffmpeg_snapshot())

    def test_legacy_schema_1_without_an_anchor_still_loads(self) -> None:
        # --update-pins loads the file before it writes it, so schema-1
        # files stay loadable: that run is what migrates them to schema 2.
        # The fetch path refuses them until then (verify_ffmpeg_snapshot).
        text = json.dumps({"schema": 1, "downloads": {"yt-dlp_linux": "a" * 64}})
        path = _write_pins(text)
        self.addCleanup(path.unlink)
        self.assertNotIn("ffmpeg_snapshot", load_pins(path))

    def test_malformed_anchor_dies(self) -> None:
        for anchor in (
            [],
            "9.0.2",
            {"version": "9.0.2", "btbn_tag": "autobuild-x"},  # member missing
            {
                "version": "9.0.2",
                "btbn_tag": "autobuild-x",
                "macos_build": "1_9.0.2",
                "extra": "x",
            },
            {"version": 902, "btbn_tag": "autobuild-x", "macos_build": "1_9.0.2"},
            {"version": "9.0.2", "btbn_tag": "", "macos_build": "1_9.0.2"},
        ):
            with self.subTest(anchor=anchor):
                path = _write_pins(
                    json.dumps(
                        {"schema": 2, "downloads": {}, "ffmpeg_snapshot": anchor}
                    )
                )
                self.addCleanup(path.unlink)
                with self.assertRaises(SystemExit):
                    load_pins(path)


class TestPinCoverage(unittest.TestCase):
    """Every artifact the script can download must be pinned; a missing pin
    fails the build rather than silently trusting the download."""

    def test_all_platforms_are_pinned(self) -> None:
        pins = load_pins()
        downloads = pins["downloads"]
        for system, machine, version in (
            ("Darwin", "arm64", TAG),
            ("Linux", "x86_64", TAG),
            ("Linux", "aarch64", TAG),
            ("Windows", "AMD64", TAG),
            ("Windows", "ARM64", TAG),
        ):
            with self.subTest(platform=f"{system}/{machine}"):
                # The pin key is the downloaded URL's basename plus the release
                # tag: the basename alone is version-independent.
                url = ytdlp_download_url(system, machine, version)
                key = ytdlp_pin_key(pin_key_from_url(url), version)
                self.assertIn(key, downloads)

    def test_btbn_archives_are_pinned(self) -> None:
        pins = load_pins()
        downloads = pins["downloads"]
        for system, machine in (
            ("Linux", "x86_64"),
            ("Linux", "aarch64"),
            ("Windows", "x86_64"),
            ("Windows", "ARM64"),
        ):
            with self.subTest(platform=f"{system}/{machine}"):
                url = ffmpeg_download_url(system, machine)
                assert url is not None
                key = pin_key_from_url(url)
                self.assertIn(key, downloads)
                self.assertRegex(key, r"-gpl-%s\." % ffmpeg_branch())

    def test_macos_zip_is_pinned(self) -> None:
        pins = load_pins()
        self.assertIn(ffmpeg_macos_artifact(), pins["downloads"])

    def test_no_superseded_ffmpeg_keys(self) -> None:
        # BtbN's names embed the build, so a bump adds keys. For the pinned
        # branch btbn_asset_name already dies on two matches, but a key from
        # another branch or a superseded macOS build would linger unnoticed.
        downloads = load_pins()["downloads"]
        self.assertEqual(
            [n for n in downloads if n.startswith("ffmpeg-macos-")],
            [ffmpeg_macos_artifact()],
        )
        btbn = [n for n in downloads if n.startswith("ffmpeg-n")]
        self.assertTrue(btbn, "the pinned BtbN assets must be in the pins file")
        for name in btbn:
            with self.subTest(name=name):
                self.assertIn(f"-gpl-{ffmpeg_branch()}.", name)

    def test_no_superseded_ytdlp_keys(self) -> None:
        # ffmpeg enforces one asset per arch (btbn_asset_name dies on two);
        # yt-dlp's equivalent is this guard: only the pinned release's keys.
        downloads = load_pins()["downloads"]
        ytdlp_keys = {name for name in downloads if name.startswith("yt-dlp")}
        expected = {
            ytdlp_pin_key(pin_key_from_url(ytdlp_download_url(system, machine, version)), version)
            for system, machine, version in (
                ("Darwin", "arm64", TAG),
                ("Linux", "x86_64", TAG),
                ("Linux", "aarch64", TAG),
                ("Windows", "AMD64", TAG),
                ("Windows", "ARM64", TAG),
            )
        }
        self.assertEqual(ytdlp_keys, expected, "superseded yt-dlp pin keys linger")


class TestFfmpegSnapshotAnchor(TmpDirTestCase):
    """Bumping FFMPEG_VERSION / FFMPEG_BTBN_TAG / FFMPEG_MACOS_BUILD without
    --update-pins must fail loudly, the way a yt-dlp bump does.

    The constants live in this script, but the pinned asset names and their
    digests come from sidecar_pins.json: after a bump the file still names
    the superseded build, so every environment — clean CI runners included —
    would silently fetch it. The snapshot identity recorded in the file is
    what turns a forgotten update into a build failure, and it is not a
    re-fetch trigger: only regenerating the file can move off the old
    snapshot.
    """

    def stale(self) -> dict:
        return {
            "version": "9.0.1",
            "btbn_tag": "autobuild-2026-01-01-00-00",
            "macos_build": "1111111111_9.0.1",
        }

    def verify_stderr(self, pins: dict) -> str:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with self.assertRaises(SystemExit):
                verify_ffmpeg_snapshot(pins)
        return stderr.getvalue()

    def test_the_current_snapshot_is_the_constants(self) -> None:
        self.assertEqual(
            ffmpeg_snapshot(),
            {
                "version": FFMPEG_VERSION,
                "btbn_tag": FFMPEG_BTBN_TAG,
                "macos_build": FFMPEG_MACOS_BUILD,
            },
        )

    def test_matching_snapshot_passes(self) -> None:
        self.assertIsNone(
            verify_ffmpeg_snapshot({"ffmpeg_snapshot": ffmpeg_snapshot()})
        )

    def test_a_snapshot_recorded_for_another_build_dies_with_the_instruction(
        self,
    ) -> None:
        stderr = self.verify_stderr({"ffmpeg_snapshot": self.stale()})
        self.assertIn("--update-pins", stderr)
        self.assertIn("autobuild-2026-01-01-00-00", stderr)  # the recorded one
        self.assertIn(FFMPEG_BTBN_TAG, stderr)  # the constants it disagrees with

    def test_no_recorded_snapshot_dies_with_the_instruction(self) -> None:
        stderr = self.verify_stderr({})
        self.assertIn("--update-pins", stderr)

    def test_repo_pins_file_matches_the_pinned_snapshot(self) -> None:
        self.assertIsNone(verify_ffmpeg_snapshot(load_pins()))

    def test_repo_pins_file_is_a_fixpoint_of_write_pins(self) -> None:
        # The committed file must be exactly what write_pins emits — the
        # anchor, schema and ordering included. Regenerating the pins is the
        # only supported repair, so code and file may never drift apart.
        repo_file = Path(__file__).resolve().parent / "sidecar_pins.json"
        out = self.tmpdir() / "sidecar_pins.json"
        write_pins(load_pins(repo_file), out)
        self.assertEqual(out.read_bytes(), repo_file.read_bytes())

    def test_main_refuses_a_stale_pins_file(self) -> None:
        stale = _write_pins(
            json.dumps({"schema": 2, "downloads": {}, "ffmpeg_snapshot": self.stale()})
        )
        self.addCleanup(stale.unlink)
        stderr = io.StringIO()
        with (
            mock.patch.object(fetch_sidecars, "PINS_FILE", stale),
            mock.patch.object(
                fetch_sidecars, "host_triple", return_value="x86_64-unknown-linux-gnu"
            ),
            mock.patch.object(
                fetch_sidecars,
                "download_and_verify",
                side_effect=AssertionError("main() must die before fetching"),
            ),
            contextlib.redirect_stderr(stderr),
        ):
            with self.assertRaises(SystemExit):
                fetch_sidecars.main()
        self.assertIn("--update-pins", stderr.getvalue())


class TestFetchFfmpegTempIsolation(unittest.TestCase):
    """--update-pins extracts five platforms' archives in one process. Each
    call must do its extraction work in a private temp dir and clean it up,
    so a later round can neither collide with nor find an earlier round's
    files."""

    def stub_download(self) -> None:
        def tar_payload(member: str, content: bytes) -> bytes:
            buf = io.BytesIO()
            with tarfile.open(fileobj=buf, mode="w:xz") as tf:
                info = tarfile.TarInfo(member)
                info.size = len(content)
                tf.addfile(info, io.BytesIO(content))
            return buf.getvalue()

        def zip_payload(member: str, content: bytes) -> bytes:
            buf = io.BytesIO()
            with zipfile.ZipFile(buf, "w") as zf:
                zf.writestr(member, content)
            return buf.getvalue()

        payloads = {
            "linux64": tar_payload("ffmpeg-archive/bin/ffmpeg", elf_header(0x3E)),
            "linuxarm64": tar_payload("ffmpeg-archive/bin/ffmpeg", elf_header(0xB7)),
            "win64": zip_payload("ffmpeg-archive/bin/ffmpeg.exe", pe_header(0x8664)),
        }

        def fake_download(
            url: str, dest: Path, pins: dict, artifact: str | None = None
        ) -> None:
            for token, payload in payloads.items():
                if f"-{token}-gpl" in url:
                    Path(dest).write_bytes(payload)
                    return
            raise AssertionError(f"unexpected url: {url}")

        original = fetch_sidecars.download_and_verify
        self.addCleanup(
            lambda: setattr(fetch_sidecars, "download_and_verify", original)
        )
        fetch_sidecars.download_and_verify = fake_download

    def test_each_call_works_in_a_private_temp_dir(self) -> None:
        self.stub_download()
        with tempfile.TemporaryDirectory() as tmp_s:
            tmp = Path(tmp_s)
            work_dirs: list[Path] = []

            original = fetch_sidecars.tempfile.mkdtemp

            def mkdtemp_in_tmp(*args: object, **kwargs: object) -> str:
                made = Path(
                    original(dir=tmp_s, prefix=kwargs.get("prefix", ""))
                )
                work_dirs.append(made)
                return str(made)

            fetch_sidecars.tempfile.mkdtemp = mkdtemp_in_tmp
            self.addCleanup(
                lambda: setattr(fetch_sidecars.tempfile, "mkdtemp", original)
            )

            out_dir = tmp / "out"
            out_dir.mkdir()
            for system, machine, name, want in (
                ("Linux", "x86_64", "ffmpeg-linux64", elf_header(0x3E)),
                ("Linux", "aarch64", "ffmpeg-linuxarm64", elf_header(0xB7)),
                ("Windows", "x86_64", "ffmpeg-win64.exe", pe_header(0x8664)),
            ):
                with self.subTest(platform=f"{system}/{machine}"):
                    out = out_dir / name
                    fetch_sidecars.fetch_ffmpeg(system, machine, out, load_pins())
                    self.assertEqual(out.read_bytes(), want)

            self.assertEqual(len(work_dirs), 3, "each call needs its own temp dir")
            self.assertEqual(len(set(work_dirs)), 3)
            for made in work_dirs:
                self.assertFalse(made.exists(), "extraction dir must be cleaned up")


class TestHttpUserAgent(unittest.TestCase):
    def test_sends_a_non_urllib_user_agent(self) -> None:
        # martin-riedl.de 403s Python-urllib's default UA; curl's works.
        with mock.patch.object(
            fetch_sidecars.urllib.request, "urlopen"
        ) as urlopen:
            fetch_sidecars._open_url("https://example.com/blob")
        request = urlopen.call_args.args[0]
        self.assertEqual(request.get_header("User-agent"), fetch_sidecars.HTTP_USER_AGENT)
        self.assertNotIn("urllib", fetch_sidecars.HTTP_USER_AGENT.lower())


def thin_arm64_header() -> bytes:
    return (
        (0xFEEDFACF).to_bytes(4, "little")
        + (0x0100000C).to_bytes(4, "little")
        + b"\x00" * 8
    )


def thin_x86_64_header() -> bytes:
    return (
        (0xFEEDFACF).to_bytes(4, "little")
        + (0x01000007).to_bytes(4, "little")
        + b"\x00" * 8
    )


def elf_header(machine: int) -> bytes:
    return (
        b"\x7fELF"
        + bytes([2, 1])  # EI_CLASS 64-bit, EI_DATA little-endian
        + b"\x00" * 12  # offsets 6..17
        + machine.to_bytes(2, "little")
        + b"\x00" * 32
    )


def pe_header(machine: int) -> bytes:
    stub = bytearray(0x40)
    stub[0:2] = b"MZ"
    stub[0x3C:0x40] = (0x40).to_bytes(4, "little")
    return bytes(stub) + b"PE\0\0" + machine.to_bytes(2, "little") + b"\x00" * 32


class TestMachoParser(unittest.TestCase):
    def test_thin_arm64_header(self) -> None:
        self.assertEqual(macho_cpu_types(thin_arm64_header()), {0x0100000C})

    def test_thin_x86_64_header(self) -> None:
        self.assertEqual(macho_cpu_types(thin_x86_64_header()), {0x01000007})

    def test_fat_header_lists_every_slice(self) -> None:
        cpus = (0x0100000C, 0x01000007)
        entries = b"".join(cpu.to_bytes(4, "big") + b"\x00" * 16 for cpu in cpus)
        data = (
            (0xCAFEBABE).to_bytes(4, "big")
            + len(cpus).to_bytes(4, "big")
            + entries
        )
        self.assertEqual(macho_cpu_types(data), set(cpus))

    def test_fat_64_header(self) -> None:
        cpu = 0x0100000C
        entry = cpu.to_bytes(4, "big") + b"\x00" * 28
        data = (0xCAFEBABF).to_bytes(4, "big") + (1).to_bytes(4, "big") + entry
        self.assertEqual(macho_cpu_types(data), {cpu})

    def test_truncated_fat_header_raises(self) -> None:
        data = (0xCAFEBABE).to_bytes(4, "big") + (2).to_bytes(4, "big") + b"\x00" * 4
        with self.assertRaises(ValueError):
            macho_cpu_types(data)

    def test_non_macho_bytes_raise(self) -> None:
        for data in (b"", b"plain text", b"\x7fELF" + b"\x00" * 12):
            with self.subTest(data=data):
                with self.assertRaises(ValueError):
                    macho_cpu_types(data)


class TestFfmpegMacosUrl(unittest.TestCase):
    def test_url_pins_the_build_id(self) -> None:
        url = ffmpeg_macos_url()
        self.assertTrue(url.startswith("https://"))
        self.assertIn(f"/{FFMPEG_MACOS_BUILD}/ffmpeg.zip", url)

    def test_never_uses_the_latest_redirect(self) -> None:
        self.assertNotIn("/redirect/", ffmpeg_macos_url())

    def test_artifact_key_carries_platform_and_build(self) -> None:
        self.assertEqual(
            ffmpeg_macos_artifact(),
            f"ffmpeg-macos-arm64-{FFMPEG_MACOS_BUILD}.zip",
        )


class TestElfParser(unittest.TestCase):
    def test_x86_64_header(self) -> None:
        self.assertEqual(elf_machine(elf_header(0x3E)), 0x3E)

    def test_aarch64_header(self) -> None:
        self.assertEqual(elf_machine(elf_header(0xB7)), 0xB7)

    def test_big_endian_header(self) -> None:
        data = bytearray(elf_header(0x3E))
        data[5] = 2
        data[18:20] = (0x3E).to_bytes(2, "big")
        self.assertEqual(elf_machine(bytes(data)), 0x3E)

    def test_32_bit_elf_raises(self) -> None:
        data = bytearray(elf_header(0x3E))
        data[4] = 1
        with self.assertRaises(ValueError):
            elf_machine(bytes(data))

    def test_pe_bytes_raise(self) -> None:
        with self.assertRaises(ValueError):
            elf_machine(pe_header(0x8664))

    def test_truncated_header_raises(self) -> None:
        with self.assertRaises(ValueError):
            elf_machine(b"\x7fELF\x02\x01")


class TestPeParser(unittest.TestCase):
    def test_x86_64_header(self) -> None:
        self.assertEqual(pe_machine(pe_header(0x8664)), 0x8664)

    def test_arm64_header(self) -> None:
        self.assertEqual(pe_machine(pe_header(0xAA64)), 0xAA64)

    def test_signature_offset_is_honoured(self) -> None:
        data = bytearray(pe_header(0x8664) + b"\x00" * 0x40)
        data[0x3C:0x40] = (0x80).to_bytes(4, "little")
        data[0x80:0x84] = b"PE\0\0"
        data[0x84:0x86] = (0xAA64).to_bytes(2, "little")
        self.assertEqual(pe_machine(bytes(data)), 0xAA64)

    def test_elf_bytes_raise(self) -> None:
        with self.assertRaises(ValueError):
            pe_machine(elf_header(0x3E))

    def test_signature_offset_past_the_end_raises(self) -> None:
        data = bytearray(pe_header(0x8664))
        data[0x3C:0x40] = (0x4000).to_bytes(4, "little")
        with self.assertRaises(ValueError):
            pe_machine(bytes(data))

    def test_missing_signature_raises(self) -> None:
        data = bytearray(pe_header(0x8664))
        data[0x40:0x44] = b"XXXX"
        with self.assertRaises(ValueError):
            pe_machine(bytes(data))

    def test_truncated_dos_header_raises(self) -> None:
        with self.assertRaises(ValueError):
            pe_machine(b"MZ" + b"\x00" * 8)


class TestVerifyBtbnFfmpegArch(TmpDirTestCase):
    def write(self, header: bytes) -> Path:
        path = self.tmpdir() / "ffmpeg"
        path.write_bytes(header)
        return path

    def test_linux_x64_elf_passes(self) -> None:
        verify_btbn_ffmpeg_arch(self.write(elf_header(0x3E)), "Linux", "x86_64")

    def test_linux_arm64_elf_passes(self) -> None:
        verify_btbn_ffmpeg_arch(self.write(elf_header(0xB7)), "Linux", "aarch64")

    def test_windows_x64_pe_passes(self) -> None:
        verify_btbn_ffmpeg_arch(self.write(pe_header(0x8664)), "Windows", "AMD64")

    def test_windows_arm64_pe_passes(self) -> None:
        verify_btbn_ffmpeg_arch(self.write(pe_header(0xAA64)), "Windows", "ARM64")

    def test_x64_elf_under_an_arm64_asset_dies(self) -> None:
        with self.assertRaises(SystemExit):
            verify_btbn_ffmpeg_arch(self.write(elf_header(0x3E)), "Linux", "aarch64")

    def test_pe_under_a_linux_asset_dies(self) -> None:
        with self.assertRaises(SystemExit):
            verify_btbn_ffmpeg_arch(self.write(pe_header(0x8664)), "Linux", "x86_64")

    def test_a_non_binary_dies(self) -> None:
        with self.assertRaises(SystemExit):
            verify_btbn_ffmpeg_arch(self.write(b"not a binary\n"), "Linux", "x86_64")

    def test_unsupported_platform_raises(self) -> None:
        with self.assertRaises(ValueError):
            verify_btbn_ffmpeg_arch(self.write(elf_header(0x3E)), "Linux", "riscv64")

    def test_an_uncovered_asset_class_dies(self) -> None:
        # A platform added to BTBN_ARCHES without an assertion entry must
        # fail loudly rather than ship unchecked.
        arches = {"Linux": dict(BTBN_ARCHES["Linux"], riscv64="linuxriscv64")}
        with mock.patch.object(fetch_sidecars, "BTBN_ARCHES", arches):
            with self.assertRaises(SystemExit):
                verify_btbn_ffmpeg_arch(
                    self.write(elf_header(0x3E)), "Linux", "riscv64"
                )


class TestBtbnArchAssertionCoverage(unittest.TestCase):
    def test_every_asset_class_has_an_assertion(self) -> None:
        for system, arches in BTBN_ARCHES.items():
            for machine, token in arches.items():
                with self.subTest(platform=f"{system}/{machine}"):
                    self.assertIn(token, fetch_sidecars._BTBN_ARCH_MACHINES)


class TestVerifyMacosFfmpeg(TmpDirTestCase):
    def write_binary(self, header: bytes) -> Path:
        path = self.tmpdir() / "ffmpeg"
        path.write_bytes(header + b"\x00" * 64)
        return path

    def patch_darwin(self, machine: str = "x86_64") -> None:
        for patcher in (
            mock.patch.object(fetch_sidecars, "detect_system", return_value="Darwin"),
            mock.patch.object(fetch_sidecars, "detect_machine", return_value=machine),
            mock.patch.object(
                fetch_sidecars.shutil, "which", return_value="/usr/bin/codesign"
            ),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    def patch_codesign(
        self,
        verify_rc: int = 0,
        team: str = FFMPEG_MACOS_TEAM_ID,
        signer: str = FFMPEG_MACOS_SIGNER,
        chain: bool = True,
        probe_rc: int = 0,
    ) -> None:
        def fake_run(cmd: list[str], **kwargs: object) -> subprocess.CompletedProcess:
            if cmd[1] == "--verify":
                return subprocess.CompletedProcess(cmd, verify_rc, stdout="", stderr="")
            if cmd[1] == "-dvvv":
                lines = [f"TeamIdentifier={team}"]
                if chain:
                    lines = [
                        f"Authority=Developer ID Application: {signer} ({team})",
                        "Authority=Developer ID Certification Authority",
                        "Authority=Apple Root CA",
                        *lines,
                    ]
                return subprocess.CompletedProcess(
                    cmd, 0, stdout="", stderr="\n".join(lines) + "\n"
                )
            if cmd[1] == "-version":
                return subprocess.CompletedProcess(
                    cmd, probe_rc, stdout="ffmpeg version 9.0.2\n", stderr=""
                )
            raise AssertionError(f"unexpected command: {cmd}")

        patcher = mock.patch.object(fetch_sidecars.subprocess, "run", side_effect=fake_run)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_accepts_arm64_binary_on_non_darwin(self) -> None:
        path = self.write_binary(thin_arm64_header())
        with mock.patch.object(fetch_sidecars, "detect_system", return_value="Linux"):
            self.assertIsNone(verify_macos_ffmpeg(path))

    def test_rejects_intel_only_binary(self) -> None:
        path = self.write_binary(thin_x86_64_header())
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg(path)

    def test_rejects_non_macho_bytes(self) -> None:
        path = self.write_binary(b"not a mach-o!")
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg(path)

    def test_darwin_checks_signature(self) -> None:
        path = self.write_binary(thin_arm64_header())
        self.patch_darwin()
        self.patch_codesign()
        self.assertIsNone(verify_macos_ffmpeg(path))

    def test_darwin_rejects_unsigned_binary(self) -> None:
        path = self.write_binary(thin_arm64_header())
        self.patch_darwin()
        self.patch_codesign(verify_rc=1)
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg(path)

    def test_darwin_rejects_foreign_team(self) -> None:
        path = self.write_binary(thin_arm64_header())
        self.patch_darwin()
        self.patch_codesign(team="OTHERTEAM")
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg(path)

    def test_darwin_rejects_signature_without_the_apple_chain(self) -> None:
        # The hole the TeamIdentifier substring match left open: the string
        # alone proves nothing about who signed.
        path = self.write_binary(thin_arm64_header())
        self.patch_darwin()
        self.patch_codesign(chain=False)
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg(path)

    def test_darwin_rejects_a_different_signer(self) -> None:
        path = self.write_binary(thin_arm64_header())
        self.patch_darwin()
        self.patch_codesign(signer="Someone Else")
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg(path)

    def test_darwin_arm64_fails_when_binary_does_not_run(self) -> None:
        path = self.write_binary(thin_arm64_header())
        self.patch_darwin(machine="arm64")
        self.patch_codesign(probe_rc=1)
        with self.assertRaises(SystemExit):
            verify_macos_ffmpeg(path)


class TestPinsRecorderDownloadGuard(TmpDirTestCase):
    """--update-pins records digests from upstream checksums before any
    download; the archive that gets extracted must match that record."""

    def make_recorder(self) -> fetch_sidecars.PinsRecorder:
        return fetch_sidecars.PinsRecorder(
            {"downloads": {}, "binaries": {}}, force=False
        )

    def patch_download(self, payload: bytes) -> None:
        def fake_download(url: str, dest: Path) -> None:
            Path(dest).write_bytes(payload)

        patcher = mock.patch.object(fetch_sidecars, "download", fake_download)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_records_a_download_that_matches_the_upstream_digest(self) -> None:
        payload = b"the pinned zip"
        digest = hashlib.sha256(payload).hexdigest()
        recorder = self.make_recorder()
        recorder.record("ffmpeg.zip", digest)
        self.patch_download(payload)
        dest = self.tmpdir() / "ffmpeg.zip"
        recorder.download_and_record(
            "https://example.invalid/ffmpeg.zip", dest, "ffmpeg.zip"
        )
        self.assertEqual(recorder.pins["downloads"]["ffmpeg.zip"], digest)

    def test_dies_when_the_download_diverges_from_the_upstream_digest(self) -> None:
        recorder = self.make_recorder()
        recorder.record("ffmpeg.zip", hashlib.sha256(b"the pinned zip").hexdigest())
        self.patch_download(b"substituted bytes")
        dest = self.tmpdir() / "ffmpeg.zip"
        with self.assertRaises(SystemExit):
            recorder.download_and_record(
                "https://example.invalid/ffmpeg.zip", dest, "ffmpeg.zip"
            )


class TestFailedDownloadsLeaveNoBytes(TmpDirTestCase):
    """A failed download or verification must not leave its bytes at the
    destination. fetch_sidecars re-checks the digest on its next run, but a
    `tauri build` in between would bundle whatever sits there — for yt-dlp,
    the destination is the final sidecar path."""

    def stub_download(self, payload: bytes) -> None:
        patcher = mock.patch.object(
            fetch_sidecars,
            "download",
            lambda url, dest: Path(dest).write_bytes(payload),
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_mismatched_download_is_removed(self) -> None:
        dest = self.tmpdir() / "yt-dlp-x86_64-unknown-linux-gnu"
        self.stub_download(b"substituted bytes")
        with self.assertRaises(SystemExit):
            fetch_sidecars.download_and_verify(
                "https://example.invalid/yt-dlp_linux",
                dest,
                {"downloads": {"yt-dlp_linux": "a" * 64}, "binaries": {}},
            )
        self.assertFalse(dest.exists(), "unverified bytes must be removed")

    def test_a_missing_pin_dies_before_any_download(self) -> None:
        dest = self.tmpdir() / "yt-dlp-x86_64-unknown-linux-gnu"
        urls: list[str] = []
        patcher = mock.patch.object(
            fetch_sidecars, "download", lambda url, dest: urls.append(url)
        )
        patcher.start()
        self.addCleanup(patcher.stop)
        with self.assertRaises(SystemExit):
            fetch_sidecars.download_and_verify(
                "https://example.invalid/yt-dlp_linux",
                dest,
                {"downloads": {}, "binaries": {}},
            )
        self.assertEqual(urls, [], "an unpinned artifact must not be downloaded")
        self.assertFalse(dest.exists())

    def test_recorder_without_an_upstream_digest_removes_the_bytes(self) -> None:
        recorder = PinsRecorder({"downloads": {}, "binaries": {}}, force=False)
        self.stub_download(b"bytes no checksum file described")
        dest = self.tmpdir() / "ffmpeg.zip"
        with self.assertRaises(SystemExit):
            recorder.download_and_record(
                "https://example.invalid/ffmpeg.zip", dest, "ffmpeg.zip"
            )
        self.assertFalse(dest.exists())

    def test_a_download_that_fails_midway_removes_the_partial_file(self) -> None:
        dest = self.tmpdir() / "yt-dlp-x86_64-unknown-linux-gnu"

        class ResetAfterFirstRead:
            reads = 0

            def read(self, size: int = -1) -> bytes:
                self.reads += 1
                if self.reads == 1:
                    return b"partial bytes"
                raise OSError("connection reset")

            def __enter__(self) -> "ResetAfterFirstRead":
                return self

            def __exit__(self, *exc: object) -> bool:
                return False

        patcher = mock.patch.object(
            fetch_sidecars,
            "_open_url",
            lambda url, headers=None: ResetAfterFirstRead(),
        )
        patcher.start()
        self.addCleanup(patcher.stop)
        with self.assertRaises(SystemExit):
            fetch_sidecars.download("https://example.invalid/yt-dlp_linux", dest)
        self.assertFalse(dest.exists())


class TestFetchFfmpegMacos(TmpDirTestCase):
    def write_zip(self, dest: Path) -> None:
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as zf:
            zf.writestr("ffmpeg", thin_arm64_header() + b"\x00" * 64)
        dest.write_bytes(buf.getvalue())

    def install_fake_download(self) -> list[tuple[str, str | None]]:
        calls: list[tuple[str, str | None]] = []

        def fake_download(
            url: str, dest: Path, pins: dict, artifact: str | None = None
        ) -> None:
            calls.append((url, artifact))
            self.write_zip(Path(dest))

        original = fetch_sidecars.download_and_verify
        fetch_sidecars.download_and_verify = fake_download
        self.addCleanup(setattr, fetch_sidecars, "download_and_verify", original)
        return calls

    def install_fake_verify(self) -> list[Path]:
        seen: list[Path] = []
        original = fetch_sidecars.verify_macos_ffmpeg
        fetch_sidecars.verify_macos_ffmpeg = lambda path: seen.append(path)
        self.addCleanup(setattr, fetch_sidecars, "verify_macos_ffmpeg", original)
        return seen

    def test_downloads_under_the_macos_pin_key(self) -> None:
        out = self.tmpdir() / "ffmpeg-aarch64-apple-darwin"
        calls = self.install_fake_download()
        seen = self.install_fake_verify()
        fetch_ffmpeg("Darwin", "arm64", out, load_pins())
        self.assertEqual(calls, [(ffmpeg_macos_url(), ffmpeg_macos_artifact())])
        self.assertEqual(seen, [out])

    def test_extracts_ffmpeg_from_the_zip(self) -> None:
        out = self.tmpdir() / "ffmpeg-aarch64-apple-darwin"
        self.install_fake_download()
        self.install_fake_verify()
        fetch_ffmpeg("Darwin", "arm64", out, load_pins())
        self.assertTrue(out.read_bytes().startswith(thin_arm64_header()[:4]))

    def test_intel_payload_is_removed(self) -> None:
        out = self.tmpdir() / "ffmpeg-aarch64-apple-darwin"

        def fake_download(
            url: str, dest: Path, pins: dict, artifact: str | None = None
        ) -> None:
            buf = io.BytesIO()
            with zipfile.ZipFile(buf, "w") as zf:
                zf.writestr("ffmpeg", thin_x86_64_header() + b"\x00" * 64)
            Path(dest).write_bytes(buf.getvalue())

        original = fetch_sidecars.download_and_verify
        fetch_sidecars.download_and_verify = fake_download
        self.addCleanup(setattr, fetch_sidecars, "download_and_verify", original)

        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Darwin", "arm64", out, load_pins())
        self.assertFalse(
            out.exists(), "rejected bytes must not stay at the sidecar path"
        )


if __name__ == "__main__":
    unittest.main()