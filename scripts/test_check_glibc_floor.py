#!/usr/bin/env python3
"""Tests for check_glibc_floor.py, assembled from synthetic ELF bytes.

The real inputs are release artifacts built on a Linux runner, and the
things this script has to get right — byte order, the extended section
count, the numeric order of versions, a chain that does not advance — are
easier to produce than to find. No ELF is checked in and no test needs a
Linux host, so the suite runs wherever the rest of scripts/ does.
"""

from __future__ import annotations

import io
import struct
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from tempfile import TemporaryDirectory

from check_glibc_floor import (
    SHT_GNU_VERNEED,
    evaluate,
    main,
    read_version_needs,
    version_tuple,
)

SHT_STRTAB = 3


def _shdr(order: str, type_: int, offset: int, size: int, link: int = 0) -> bytes:
    section = bytearray(64)
    struct.pack_into(order + "I", section, 0x04, type_)
    struct.pack_into(order + "Q", section, 0x18, offset)
    struct.pack_into(order + "Q", section, 0x20, size)
    struct.pack_into(order + "I", section, 0x28, link)
    return bytes(section)


def _build(
    needs: list[tuple[str, list[str]]],
    *,
    order: str = "<",
    extended_count: bool = False,
    with_verneed: bool = True,
    gap: int = 0,
    aux_pad: int = 0,
) -> tuple[bytearray, int, int]:
    """A 64-bit ELF carrying `needs` as [(library, [versions])].

    `gap` pads each entry with that many zero bytes. Entries are laid out
    contiguously without it, which makes the next-pointer's two readings
    (relative to the entry it sits in, or absolute) agree on the first hop:
    a fixture with a gap is what tells them apart. `aux_pad` does the same
    for the aux chain, by pushing the first aux entry off the offset an
    absolute reading would compute.

    Returns the bytes, the offset of .gnu.version_r and the offset of the
    section header table, so tests can damage a specific field.
    """
    strings = bytearray(b"\0")
    offsets: dict[str, int] = {}

    def intern(text: str) -> int:
        if text not in offsets:
            offsets[text] = len(strings)
            strings.extend(text.encode() + b"\0")
        return offsets[text]

    entries = [(intern(lib), [intern(v) for v in versions]) for lib, versions in needs]

    verneed = bytearray()
    starts: list[int] = []
    for file_off, versions in entries:
        starts.append(len(verneed))
        count = len(versions)
        verneed += struct.pack(
            order + "HHIII", 1, count, file_off, 16 + aux_pad if count else 0, 0
        )
        verneed += bytes(aux_pad)
        for index, name_off in enumerate(versions):
            verneed += struct.pack(
                order + "IHHII",
                0,
                0,
                index + 2,
                name_off,
                16 if index + 1 < count else 0,
            )
        verneed += bytes(gap)
    for index, start in enumerate(starts):
        following = starts[index + 1] - start if index + 1 < len(starts) else 0
        struct.pack_into(order + "I", verneed, start + 12, following)

    dynstr_off = 64
    verneed_off = dynstr_off + len(strings)
    shoff = (verneed_off + len(verneed) + 7) // 8 * 8

    sections = [bytes(64), _shdr(order, SHT_STRTAB, dynstr_off, len(strings))]
    if with_verneed:
        sections.append(
            _shdr(order, SHT_GNU_VERNEED, verneed_off, len(verneed), link=1)
        )
    header_count = len(sections)
    if extended_count:
        sections[0] = _shdr(order, 0, 0, len(sections))
        header_count = 0

    ehdr = bytearray(64)
    ehdr[0:4] = b"\x7fELF"
    ehdr[4] = 2  # 64-bit
    ehdr[5] = 1 if order == "<" else 2
    struct.pack_into(order + "H", ehdr, 0x10, 3)  # ET_DYN
    struct.pack_into(order + "H", ehdr, 0x12, 0x3E)  # EM_X86_64
    struct.pack_into(order + "Q", ehdr, 0x28, shoff)
    struct.pack_into(order + "H", ehdr, 0x3A, 64)  # e_shentsize
    struct.pack_into(order + "H", ehdr, 0x3C, header_count)

    data = bytearray(ehdr)
    data += strings
    assert len(data) == verneed_off
    data += verneed
    data += bytes(shoff - len(data))
    for section in sections:
        data += section
    return data, verneed_off, shoff


def _elf(needs: list[tuple[str, list[str]]], **kwargs) -> bytes:
    data, _, _ = _build(needs, **kwargs)
    return bytes(data)


class VersionTupleTest(unittest.TestCase):
    def test_reads_numbers(self) -> None:
        self.assertEqual(version_tuple("GLIBC_2.34"), (2, 34))
        self.assertEqual(version_tuple("GLIBC_2.2.5"), (2, 2, 5))

    def test_other_libraries_are_not_glibc(self) -> None:
        # GLIBCXX_ is libstdc++'s scheme; the underscore after GLIBC is what
        # separates it from libc's.
        self.assertIsNone(version_tuple("GLIBCXX_3.4.29"))
        self.assertIsNone(version_tuple("GCC_3.0"))
        self.assertIsNone(version_tuple("CXXABI_1.3"))

    def test_names_without_a_number(self) -> None:
        self.assertIsNone(version_tuple("GLIBC_PRIVATE"))
        self.assertIsNone(version_tuple("GLIBC_"))
        self.assertIsNone(version_tuple("GLIBC_2.x"))


class ReadVersionNeedsTest(unittest.TestCase):
    def test_reads_every_library_and_version(self) -> None:
        data = _elf(
            [
                ("libc.so.6", ["GLIBC_2.34", "GLIBC_2.2.5"]),
                ("libgcc_s.so.1", ["GCC_3.0"]),
            ]
        )
        self.assertEqual(
            read_version_needs(data),
            [
                ("libc.so.6", "GLIBC_2.34"),
                ("libc.so.6", "GLIBC_2.2.5"),
                ("libgcc_s.so.1", "GCC_3.0"),
            ],
        )

    def test_walks_a_chain_of_three_libraries_with_gaps(self) -> None:
        # The third entry is the point: with only two, the first hop's
        # next-pointer reads the same whether it is taken as relative to the
        # entry it sits in or as an absolute offset, so a parser that got
        # that wrong would still agree with the fixture. The gap makes the
        # second hop land in padding unless the relative reading is used.
        data = _elf(
            [
                ("libc.so.6", ["GLIBC_2.34", "GLIBC_2.2.5"]),
                ("libm.so.6", ["GLIBC_2.29"]),
                ("libpthread.so.0", ["GLIBC_2.3.2"]),
            ],
            gap=64,
        )
        self.assertEqual(
            read_version_needs(data),
            [
                ("libc.so.6", "GLIBC_2.34"),
                ("libc.so.6", "GLIBC_2.2.5"),
                ("libm.so.6", "GLIBC_2.29"),
                ("libpthread.so.0", "GLIBC_2.3.2"),
            ],
        )

    def test_reads_an_aux_chain_relative_to_its_own_entry(self) -> None:
        # vn_aux is relative to the need entry it sits in and vna_next to the
        # aux entry it sits in. The padding makes an absolute reading land in
        # zeroes and pick up a version-less name instead of the second one.
        data = _elf([("libc.so.6", ["GLIBC_2.34", "GLIBC_2.2.5"])], aux_pad=16)
        self.assertEqual(
            read_version_needs(data),
            [("libc.so.6", "GLIBC_2.34"), ("libc.so.6", "GLIBC_2.2.5")],
        )

    def test_big_endian(self) -> None:
        data = _elf([("libc.so.6", ["GLIBC_2.34"])], order=">")
        self.assertEqual(read_version_needs(data), [("libc.so.6", "GLIBC_2.34")])

    def test_extended_section_count(self) -> None:
        data = _elf([("libc.so.6", ["GLIBC_2.34"])], extended_count=True)
        self.assertEqual(read_version_needs(data), [("libc.so.6", "GLIBC_2.34")])

    def test_no_version_need_section_is_not_an_error(self) -> None:
        self.assertEqual(read_version_needs(_elf([], with_verneed=False)), [])

    def test_rejects_non_elf(self) -> None:
        with self.assertRaisesRegex(ValueError, "not an ELF file"):
            read_version_needs(b"MZ\x90\x00" + bytes(64))

    def test_rejects_32_bit(self) -> None:
        data = bytearray(_elf([("libc.so.6", ["GLIBC_2.34"])]))
        data[4] = 1
        with self.assertRaisesRegex(ValueError, "not a 64-bit ELF file"):
            read_version_needs(bytes(data))

    def test_rejects_missing_section_headers(self) -> None:
        data = bytearray(_elf([("libc.so.6", ["GLIBC_2.34"])]))
        struct.pack_into("<Q", data, 0x28, 0)
        with self.assertRaisesRegex(ValueError, "no section headers"):
            read_version_needs(bytes(data))

    def test_rejects_section_table_past_the_end(self) -> None:
        data, _, _ = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<Q", data, 0x28, len(data) - 8)
        with self.assertRaisesRegex(ValueError, "runs past the end of the file"):
            read_version_needs(bytes(data))

    def test_rejects_section_out_of_bounds(self) -> None:
        data, _, shoff = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<Q", data, shoff + 2 * 64 + 0x18, len(data) - 4)
        with self.assertRaisesRegex(ValueError, "out of bounds"):
            read_version_needs(bytes(data))

    def test_rejects_string_table_out_of_bounds(self) -> None:
        data, _, shoff = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<Q", data, shoff + 1 * 64 + 0x20, len(data))
        with self.assertRaisesRegex(ValueError, "out of bounds"):
            read_version_needs(bytes(data))

    def test_rejects_truncated_header(self) -> None:
        data = _elf([("libc.so.6", ["GLIBC_2.34"])])
        with self.assertRaisesRegex(ValueError, "truncated ELF header"):
            read_version_needs(data[:32])

    def test_rejects_unexpected_section_header_size(self) -> None:
        data, _, _ = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<H", data, 0x3A, 32)
        with self.assertRaisesRegex(ValueError, "unexpected section header size 32"):
            read_version_needs(bytes(data))

    def test_rejects_version_section_without_a_string_table(self) -> None:
        data, _, shoff = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<I", data, shoff + 2 * 64 + 0x28, 99)
        with self.assertRaisesRegex(ValueError, "string table that is not here"):
            read_version_needs(bytes(data))

    def test_rejects_section_smaller_than_one_entry(self) -> None:
        data, _, shoff = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<Q", data, shoff + 2 * 64 + 0x20, 8)
        with self.assertRaisesRegex(ValueError, "runs past the end of its section"):
            read_version_needs(bytes(data))

    def test_rejects_aux_entry_past_the_section(self) -> None:
        data, verneed_off, _ = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<I", data, verneed_off + 8, 0xFFFF)  # vn_aux
        with self.assertRaisesRegex(ValueError, "auxiliary entry runs past its section"):
            read_version_needs(bytes(data))

    def test_rejects_name_past_the_string_table(self) -> None:
        data, verneed_off, _ = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<I", data, verneed_off + 16 + 8, 0xFFFF)  # vna_name
        with self.assertRaisesRegex(ValueError, "points past the end of its string table"):
            read_version_needs(bytes(data))

    def test_rejects_need_chain_that_does_not_advance(self) -> None:
        data, verneed_off, _ = _build(
            [("libc.so.6", ["GLIBC_2.34"]), ("libm.so.6", ["GLIBC_2.29"])]
        )
        struct.pack_into("<I", data, verneed_off + 12, 8)
        with self.assertRaisesRegex(ValueError, "chain does not advance"):
            read_version_needs(bytes(data))

    def test_rejects_aux_chain_that_does_not_advance(self) -> None:
        data, verneed_off, _ = _build(
            [("libc.so.6", ["GLIBC_2.34", "GLIBC_2.2.5"])]
        )
        struct.pack_into("<I", data, verneed_off + 16 + 12, 8)
        with self.assertRaisesRegex(ValueError, "auxiliary chain does not advance"):
            read_version_needs(bytes(data))

    def test_rejects_count_larger_than_the_chain(self) -> None:
        data, verneed_off, _ = _build([("libc.so.6", ["GLIBC_2.34"])])
        struct.pack_into("<H", data, verneed_off + 2, 5)
        with self.assertRaisesRegex(ValueError, "lists 5 needs .* but holds 1"):
            read_version_needs(bytes(data))

    def test_rejects_unterminated_version_name(self) -> None:
        data, verneed_off, _ = _build([("libc.so.6", ["GLIBC_2.34"])])
        # The string table ends where .gnu.version_r begins; its last byte is
        # the NUL closing the only name here.
        data[verneed_off - 1] = ord("X")
        with self.assertRaisesRegex(ValueError, "not terminated"):
            read_version_needs(bytes(data))


class EvaluateTest(unittest.TestCase):
    def test_floor_is_the_highest_glibc_version(self) -> None:
        floor, too_new, unreadable = evaluate(
            [("libc.so.6", "GLIBC_2.2.5"), ("libc.so.6", "GLIBC_2.34")], (2, 34)
        )
        self.assertEqual(floor, (2, 34))
        self.assertEqual(too_new, [])
        self.assertEqual(unreadable, [])

    def test_versions_compare_as_numbers_not_text(self) -> None:
        # 2.9 predates 2.34, though "2.9" sorts after "2.34" as text.
        floor, too_new, _ = evaluate([("libc.so.6", "GLIBC_2.9")], (2, 34))
        self.assertEqual(floor, (2, 9))
        self.assertEqual(too_new, [])

    def test_other_libraries_are_ignored(self) -> None:
        floor, too_new, unreadable = evaluate(
            [
                ("libstdc++.so.6", "GLIBCXX_3.4.29"),
                ("libgcc_s.so.1", "GCC_4.2.0"),
                ("libc.so.6", "CXXABI_1.3"),
            ],
            (2, 34),
        )
        self.assertIsNone(floor)
        self.assertEqual(too_new, [])
        self.assertEqual(unreadable, [])

    def test_newer_than_the_ceiling_is_reported_per_library(self) -> None:
        floor, too_new, unreadable = evaluate(
            [("libc.so.6", "GLIBC_2.38"), ("libm.so.6", "GLIBC_2.35")], (2, 34)
        )
        self.assertEqual(floor, (2, 38))
        self.assertEqual(too_new, [("libc.so.6", "GLIBC_2.38"), ("libm.so.6", "GLIBC_2.35")])
        self.assertEqual(unreadable, [])

    def test_relr_needs_the_glibc_that_introduced_it(self) -> None:
        floor, too_new, unreadable = evaluate([("libc.so.6", "GLIBC_ABI_DT_RELR")], (2, 34))
        self.assertEqual(floor, (2, 36))
        self.assertEqual(too_new, [("libc.so.6", "GLIBC_ABI_DT_RELR")])
        self.assertEqual(unreadable, [])

    def test_unorderable_name_fails_rather_than_passes(self) -> None:
        floor, too_new, unreadable = evaluate([("libc.so.6", "GLIBC_FROM_THE_FUTURE")], (2, 34))
        self.assertIsNone(floor)
        self.assertEqual(too_new, [])
        self.assertEqual(unreadable, [("libc.so.6", "GLIBC_FROM_THE_FUTURE")])


class MainTest(unittest.TestCase):
    def run_main(self, files: dict[str, bytes], *args: str) -> tuple[int, str]:
        with TemporaryDirectory() as tmp:
            paths = []
            for name, data in files.items():
                path = Path(tmp) / name
                path.write_bytes(data)
                paths.append(str(path))
            out = io.StringIO()
            with redirect_stdout(out):
                code = main([*args, *paths])
        return code, out.getvalue()

    def test_passing_binary_reports_its_floor(self) -> None:
        code, out = self.run_main(
            {"video-fetch": _elf([("libc.so.6", ["GLIBC_2.34", "GLIBC_2.2.5"])])},
            "--max",
            "2.34",
        )
        self.assertEqual(code, 0)
        self.assertIn("glibc floor 2.34", out)
        self.assertIn("x86-64", out)

    def test_ceiling_is_inclusive(self) -> None:
        code, _ = self.run_main(
            {"video-fetch": _elf([("libc.so.6", ["GLIBC_2.34"])])}, "--max", "2.34"
        )
        self.assertEqual(code, 0)

    def test_binary_without_version_needs_passes(self) -> None:
        code, out = self.run_main(
            {"video-fetch": _elf([], with_verneed=False)}, "--max", "2.34"
        )
        self.assertEqual(code, 0)
        self.assertIn("nothing to check", out)

    def test_too_new_version_fails_and_names_the_library(self) -> None:
        code, out = self.run_main(
            {"video-fetch": _elf([("libc.so.6", ["GLIBC_2.38"])])}, "--max", "2.34"
        )
        self.assertEqual(code, 1)
        self.assertIn("GLIBC_2.38 required by libc.so.6", out)
        self.assertIn("FAIL", out)

    def test_unorderable_version_fails(self) -> None:
        code, out = self.run_main(
            {"video-fetch": _elf([("libc.so.6", ["GLIBC_PRIVATE"])])}, "--max", "2.34"
        )
        self.assertEqual(code, 1)
        self.assertIn("carries no version this script", out)
        # A runner scanning the log has to see a failure, not just an
        # indented note under an otherwise healthy-looking floor line.
        self.assertIn("FAIL", out)

    def test_every_binary_is_checked_not_just_the_first_failure(self) -> None:
        code, out = self.run_main(
            {
                "good": _elf([("libc.so.6", ["GLIBC_2.34"])]),
                "bad": _elf([("libc.so.6", ["GLIBC_2.39"])]),
            },
            "--max",
            "2.34",
        )
        self.assertEqual(code, 1)
        self.assertIn("good", out)
        self.assertIn("GLIBC_2.39 required by libc.so.6", out)

    def test_unreadable_file_fails(self) -> None:
        code, out = self.run_main({"video-fetch": b"MZ\x90\x00" + bytes(64)}, "--max", "2.34")
        self.assertEqual(code, 1)
        self.assertIn("not an ELF file", out)

    def test_missing_file_fails(self) -> None:
        with TemporaryDirectory() as tmp:
            out = io.StringIO()
            with redirect_stdout(out):
                code = main(["--max", "2.34", str(Path(tmp) / "absent")])
        self.assertEqual(code, 1)
        self.assertIn("ERROR", out.getvalue())

    def test_max_is_required(self) -> None:
        self.assert_rejected(["video-fetch"])

    def test_max_must_be_a_version(self) -> None:
        self.assert_rejected(["--max", "latest", "video-fetch"])

    def test_max_rejects_a_non_numeric_component(self) -> None:
        self.assert_rejected(["--max", "2.x", "video-fetch"])

    def assert_rejected(self, argv: list[str]) -> None:
        # argparse prints its usage before exiting; keep it out of the log.
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            main(argv)


if __name__ == "__main__":
    unittest.main()
