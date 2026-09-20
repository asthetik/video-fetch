#!/usr/bin/env python3
from __future__ import annotations

import io
import shutil
import tarfile
import tempfile
import unittest
from collections.abc import Sequence
from pathlib import Path
from unittest import mock

import fetch_sidecars
from fetch_sidecars import (
    FFMPEG_VERSION,
    extract_tar_xz,
    fetch_ffmpeg,
    ffmpeg_branch,
    ffmpeg_download_url,
    load_ytdlp_version,
    normalize_ytdlp_version,
    ytdlp_download_url,
)

PIN = "2026.8.19"
TAG = "2026.08.19"


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


class TestExtractTarXz(TmpDirTestCase):
    def write_archive(self, name: str, data: bytes = b"binary\n") -> Path:
        archive = self.tmpdir() / "ffmpeg.tar.xz"
        write_tar_xz(archive, files=[(name, data)])
        return archive

    def test_extracts_nested_binary(self) -> None:
        archive = self.write_archive("ffmpeg-n9.0-latest-linux64-gpl-9.0/bin/ffmpeg")
        dest = self.tmpdir()
        extract_tar_xz(archive, dest)
        out = dest / "ffmpeg-n9.0-latest-linux64-gpl-9.0" / "bin" / "ffmpeg"
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
        write_tar_xz(
            archive,
            links=[("ffmpeg-n9.0-latest-linux64-gpl-9.0/bin/ffmpeg", "../../../outside")],
        )
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
        def fake_download(url: str, dest: Path) -> None:
            write_tar_xz(dest, files=members)

        original = fetch_sidecars.download
        fetch_sidecars.download = fake_download
        self.addCleanup(setattr, fetch_sidecars, "download", original)

    def test_prefers_bin_ffmpeg_from_archive(self) -> None:
        tmp = self.tmpdir()
        out = tmp / "ffmpeg-x86_64-unknown-linux-gnu"
        self.install_fake_download(
            [
                ("ffmpeg-n9.0-latest-linux64-gpl-9.0/bin/ffmpeg", b"real\n"),
                ("ffmpeg-n9.0-latest-linux64-gpl-9.0/share/ffmpeg", b"decoy\n"),
            ]
        )
        fetch_ffmpeg("Linux", "x86_64", out, tmp)
        self.assertEqual(out.read_bytes(), b"real\n")

    def test_archive_without_bin_ffmpeg_dies(self) -> None:
        tmp = self.tmpdir()
        out = tmp / "ffmpeg-x86_64-unknown-linux-gnu"
        self.install_fake_download(
            [("ffmpeg-n9.0-latest-linux64-gpl-9.0/share/ffmpeg", b"decoy\n")]
        )
        with self.assertRaises(SystemExit):
            fetch_ffmpeg("Linux", "x86_64", out, tmp)


if __name__ == "__main__":
    unittest.main()
