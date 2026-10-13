#!/usr/bin/env python3
"""Assert that a built binary keeps the glibc floor the release promises.

The Linux bundles are built on ubuntu-22.04 on purpose, and what matters
about that image is the libc under it: glibc decides the oldest symbol
version the artifact needs, and that number is the compatibility promise —
Ubuntu 22.04, Debian 12 and RHEL 9 can run the bundles because nothing in
them asks for a symbol newer than RHEL 9's glibc 2.34. Nothing else in CI
constrains it. Bumping tauri or a webview binding can raise the floor
without touching a workflow, and the first evidence would be a user on
RHEL 9 whose app does not start.

The floor is read from `.gnu.version_r`, the version needs the linker
recorded for the symbols the binary imports from its shared libraries —
the ones ld.so resolves before `main` runs. That section survives `strip`
(it belongs to the dynamic segment, not to the symbol table), so it can be
read from the artifacts the release actually builds.

    python scripts/check_glibc_floor.py --max 2.34 src-tauri/target/release/video-fetch

Prints the floor each binary needs and exits 0 when all of them are at or
below --max; exits 1, naming the versions at fault, otherwise. A need this
script cannot order counts as a failure too, never as a warning: an answer
it cannot read must not pass for an answer it liked.
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from fetch_sidecars import elf_machine  # noqa: E402

# Section type of the version-need section (GNU extension to the ELF spec).
SHT_GNU_VERNEED = 0x6FFFFFFE

# Elf64_Shdr is fixed-size in a 64-bit file; a different stride means this
# is not the kind of object this script was written against.
SHDR_SIZE = 64

# Version needs whose name carries no number but still says something about
# the floor: packed relative relocations arrived with glibc 2.36, so a
# binary that asks for them needs that glibc or newer. Mapping it to a
# version keeps it comparable instead of leaving it unorderable; the
# direction of a mistake here is safe, since any value above the ceiling
# fails the check rather than passing it.
NAMED_NEEDS = {"GLIBC_ABI_DT_RELR": (2, 36)}

# e_machine values worth naming in the log; the release builds x86-64 and
# AArch64 Linux, so anything else is worth seeing spelled out.
_MACHINES = {0x3E: "x86-64", 0xB7: "AArch64", 0xF3: "RISC-V"}


def version_tuple(name: str) -> tuple[int, ...] | None:
    """(2, 34) from 'GLIBC_2.34', or None when the tail is not all numbers.

    Compared as numbers, never as text: '2.9' is older than '2.34', while a
    string compare would call it newer.
    """
    if not name.startswith("GLIBC_"):
        return None
    parts = name[len("GLIBC_") :].split(".")
    if not all(part.isascii() and part.isdigit() for part in parts):
        return None
    return tuple(int(part) for part in parts)


def _string(data: bytes, str_off: int, str_size: int, name_off: int) -> str:
    """One NUL-terminated name out of the version string table."""
    if name_off >= str_size:
        raise ValueError("a version name points past the end of its string table")
    start = str_off + name_off
    end = data.find(b"\0", start, str_off + str_size)
    if end < 0:
        raise ValueError("a version name is not terminated inside its string table")
    return data[start:end].decode("utf-8", "replace")


def _read_verneed(
    data: bytes, order: str, offset: int, size: int, str_off: int, str_size: int
) -> list[tuple[str, str]]:
    """(library, version) for every need in one `.gnu.version_r` section.

    Both chains are walked by their own next-pointers and each pointer must
    move forward, so a corrupt section cannot spin here; the aux count is
    checked against what the chain actually holds rather than trusted to
    bound the walk, since a count larger than the chain would otherwise
    hide entries.
    """
    end = offset + size
    needs: list[tuple[str, str]] = []
    at = 0
    while True:
        if offset + at + 16 > end:
            raise ValueError(".gnu.version_r runs past the end of its section")
        _, count, file_off, aux, next_need = struct.unpack_from(
            order + "HHIII", data, offset + at
        )
        library = _string(data, str_off, str_size, file_off)
        aux_at = at + aux
        seen = 0
        while aux_at:
            if offset + aux_at + 16 > end:
                raise ValueError(
                    "a .gnu.version_r auxiliary entry runs past its section"
                )
            _, _, _, name_off, aux_next = struct.unpack_from(
                order + "IHHII", data, offset + aux_at
            )
            needs.append((library, _string(data, str_off, str_size, name_off)))
            seen += 1
            if not aux_next:
                break
            if aux_next < 16:
                raise ValueError("a .gnu.version_r auxiliary chain does not advance")
            aux_at += aux_next
        if seen != count:
            raise ValueError(
                f".gnu.version_r lists {count} needs for {library} "
                f"but holds {seen}"
            )
        if not next_need:
            break
        if next_need < 16:
            raise ValueError("the .gnu.version_r chain does not advance")
        at += next_need
    return needs


def read_version_needs(data: bytes) -> list[tuple[str, str]]:
    """(library, version) for every version need in a 64-bit ELF.

    Raises ValueError when the bytes are not a 64-bit ELF or when the
    section headers cannot be trusted. An empty list means the file records
    no version needs at all — a static or unversioned binary needs nothing
    from glibc, which is a pass, not an unknown.
    """
    elf_machine(data)  # raises unless this is a 64-bit ELF
    if len(data) < 64:
        raise ValueError("truncated ELF header")
    # struct wants a prefix, int.from_bytes wants a name; keep both in step.
    order = "<" if data[5] == 1 else ">"
    byte_order = "little" if order == "<" else "big"

    shoff = int.from_bytes(data[0x28:0x30], byte_order)
    shentsize = int.from_bytes(data[0x3A:0x3C], byte_order)
    shnum = int.from_bytes(data[0x3C:0x3E], byte_order)
    if shoff == 0:
        raise ValueError("no section headers, so no .gnu.version_r to read")
    if shentsize != SHDR_SIZE:
        raise ValueError(f"unexpected section header size {shentsize}")
    if shnum == 0:
        # ELF's escape hatch: when the count does not fit in the header
        # field, the real one lives in section 0's sh_size.
        shnum = int.from_bytes(data[shoff + 0x20 : shoff + 0x28], byte_order)
    if shoff + shnum * SHDR_SIZE > len(data):
        raise ValueError("the section header table runs past the end of the file")

    sections: list[tuple[int, int, int, int]] = []
    for index in range(shnum):
        at = shoff + index * SHDR_SIZE
        sections.append(
            (
                int.from_bytes(data[at + 0x04 : at + 0x08], byte_order),  # sh_type
                int.from_bytes(data[at + 0x18 : at + 0x20], byte_order),  # sh_offset
                int.from_bytes(data[at + 0x20 : at + 0x28], byte_order),  # sh_size
                int.from_bytes(data[at + 0x28 : at + 0x2C], byte_order),  # sh_link
            )
        )

    needs: list[tuple[str, str]] = []
    for section_type, offset, size, link in sections:
        if section_type != SHT_GNU_VERNEED:
            continue
        if link >= len(sections):
            raise ValueError(".gnu.version_r names a string table that is not here")
        _, str_off, str_size, _ = sections[link]
        if str_off + str_size > len(data) or offset + size > len(data):
            raise ValueError(".gnu.version_r or its string table is out of bounds")
        needs.extend(_read_verneed(data, order, offset, size, str_off, str_size))
    return needs


def evaluate(
    needs: list[tuple[str, str]], ceiling: tuple[int, ...]
) -> tuple[tuple[int, ...] | None, list[tuple[str, str]], list[tuple[str, str]]]:
    """Sort one binary's needs into (floor, too new, unreadable).

    GCC_, GLIBCXX_ and CXXABI_ version their own libraries and say nothing
    about glibc; only the `GLIBC_` prefix is read, and only with the
    underscore — `GLIBCXX_3.4.29` is libstdc++, not libc.
    """
    floor: tuple[int, ...] | None = None
    too_new: list[tuple[str, str]] = []
    unreadable: list[tuple[str, str]] = []
    for library, name in needs:
        if not name.startswith("GLIBC_"):
            continue
        version = version_tuple(name)
        if version is None:
            version = NAMED_NEEDS.get(name)
        if version is None:
            unreadable.append((library, name))
            continue
        if floor is None or version > floor:
            floor = version
        if version > ceiling:
            too_new.append((library, name))
    return floor, too_new, unreadable


def check(path: Path, ceiling: tuple[int, ...]) -> bool:
    """Report one binary's floor; True when it clears the ceiling."""
    try:
        data = path.read_bytes()
        machine = elf_machine(data)
        floor, too_new, unreadable = evaluate(read_version_needs(data), ceiling)
    except OSError as exc:
        print(f"ERROR {path}: {exc}")
        return False
    except ValueError as exc:
        print(f"ERROR {path}: {exc}")
        return False

    arch = _MACHINES.get(machine, f"e_machine {machine:#x}")
    ceiling_text = ".".join(str(part) for part in ceiling)
    if floor is None and not unreadable:
        print(f"{path} ({arch}): records no version needs; nothing to check")
        return True
    floor_text = ".".join(str(part) for part in floor) if floor else "unknown"
    print(f"{path} ({arch}): glibc floor {floor_text}")

    for library, name in too_new:
        print(f"  {name} required by {library}, above the {ceiling_text} ceiling")
    for library, name in unreadable:
        print(
            f"  {name} required by {library}, which this script cannot order "
            "— teach it the version before trusting the floor"
        )
    if too_new:
        print(
            f"FAIL {path}: this binary does not run on the oldest supported "
            f"distribution; the {ceiling_text} floor is what the CHANGELOG promises"
        )
    return not too_new and not unreadable


def _ceiling(text: str) -> tuple[int, ...]:
    parts = text.split(".")
    if not parts or not all(part.isascii() and part.isdigit() for part in parts):
        raise argparse.ArgumentTypeError(f"expected a version like 2.34, got {text!r}")
    return tuple(int(part) for part in parts)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--max",
        type=_ceiling,
        required=True,
        dest="ceiling",
        metavar="X.Y",
        help="oldest glibc the bundles must run on, e.g. 2.34 for RHEL 9",
    )
    parser.add_argument("binaries", nargs="+", metavar="BINARY", type=Path)
    args = parser.parse_args(argv)

    # Every binary is checked, not just up to the first failure: a release
    # cell builds one, but a local run over several should report them all.
    ok = True
    for path in args.binaries:
        ok = check(path, args.ceiling) and ok
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
