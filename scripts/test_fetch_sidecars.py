#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import io
import json
import shutil
import tarfile
import tempfile
import unittest
import zipfile
from collections.abc import Sequence
from pathlib import Path
from unittest import mock

import fetch_sidecars
from fetch_sidecars import (
    FFMPEG_BTBN_TAG,
    FFMPEG_VERSION,
    extract_tar_xz,
    fetch_ffmpeg,
    ffmpeg_branch,
    ffmpeg_download_url,
    load_pins,
    load_ytdlp_version,
    normalize_ytdlp_version,
    parse_shasums,
    pin_key_from_url,
    sha256_hex,
    verify_sha256,
    ytdlp_download_url,
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
        def fake_download(url: str, dest: Path, pins: dict) -> None:
            write_tar_xz(dest, files=members)

        original = fetch_sidecars.download_and_verify
        fetch_sidecars.download_and_verify = fake_download
        self.addCleanup(setattr, fetch_sidecars, "download_and_verify", original)

    def test_prefers_bin_ffmpeg_from_archive(self) -> None:
        out = self.tmpdir() / "ffmpeg-x86_64-unknown-linux-gnu"
        self.install_fake_download(
            [
                ("ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/bin/ffmpeg", b"real\n"),
                ("ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/share/ffmpeg", b"decoy\n"),
            ]
        )
        fetch_ffmpeg("Linux", "x86_64", out, {"downloads": {}})
        self.assertEqual(out.read_bytes(), b"real\n")

    def test_archive_without_bin_ffmpeg_dies(self) -> None:
        out = self.tmpdir() / "ffmpeg-x86_64-unknown-linux-gnu"
        self.install_fake_download(
            [("ffmpeg-n9.0.2-3-ga5923073bf-linux64-gpl-9.0/share/ffmpeg", b"decoy\n")]
        )
        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Linux", "x86_64", out, {"downloads": {}})


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
            parse_shasums(f"# header\n\n{'d' * 64}  ffmpeg-9.0.1.zip\n"),
            {"ffmpeg-9.0.1.zip": "d" * 64},
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
                self.assertRegex(
                    name,
                    r"^(yt-dlp(_macos|_linux|_linux_aarch64|_arm64)?\.exe"
                    r"|yt-dlp_(macos|linux|linux_aarch64)"
                    r"|ffmpeg-.*-(linux64|linuxarm64|win64|winarm64)-gpl-.*"
                    r"|ffmpeg-\d+\.\d+\.\d+\.zip)$",
                    "unexpected sidecar artifact name",
                )
                self.assertRegex(digest, r"^[0-9a-f]{64}$")

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
                key = pin_key_from_url(ytdlp_download_url(system, machine, version))
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

    def test_evermeet_zip_is_pinned(self) -> None:
        pins = load_pins()
        self.assertIn(f"ffmpeg-{FFMPEG_VERSION}.zip", pins["downloads"])


class TestFetchFfmpegTempIsolation(unittest.TestCase):
    """--update-pins extracts five platforms' archives in one process. Each
    call must do its extraction work in a private temp dir and clean it up,
    so a later round can neither collide with nor find an earlier round's
    files."""

    def stub_download(self, content: bytes) -> None:
        tar_buf = io.BytesIO()
        with tarfile.open(fileobj=tar_buf, mode="w:xz") as tf:
            info = tarfile.TarInfo("ffmpeg-archive/bin/ffmpeg")
            info.size = len(content)
            tf.addfile(info, io.BytesIO(content))
        zip_buf = io.BytesIO()
        with zipfile.ZipFile(zip_buf, "w") as zf:
            zf.writestr("ffmpeg-archive/bin/ffmpeg.exe", content)
        tar_payload = tar_buf.getvalue()
        zip_payload = zip_buf.getvalue()

        def fake_download(url: str, dest: Path, pins: dict) -> None:
            Path(dest).write_bytes(
                zip_payload if url.endswith(".zip") else tar_payload
            )

        original = fetch_sidecars.download_and_verify
        self.addCleanup(
            lambda: setattr(fetch_sidecars, "download_and_verify", original)
        )
        fetch_sidecars.download_and_verify = fake_download

    def test_each_call_works_in_a_private_temp_dir(self) -> None:
        self.stub_download(b"payload")
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
            for system, machine, name in (
                ("Linux", "x86_64", "ffmpeg-linux64"),
                ("Linux", "aarch64", "ffmpeg-linuxarm64"),
                ("Windows", "x86_64", "ffmpeg-win64.exe"),
            ):
                with self.subTest(platform=f"{system}/{machine}"):
                    out = out_dir / name
                    fetch_sidecars.fetch_ffmpeg(system, machine, out, {"downloads": {}})
                    self.assertEqual(out.read_bytes(), b"payload")

            self.assertEqual(len(work_dirs), 3, "each call needs its own temp dir")
            self.assertEqual(len(set(work_dirs)), 3)
            for made in work_dirs:
                self.assertFalse(made.exists(), "extraction dir must be cleaned up")


if __name__ == "__main__":
    unittest.main()