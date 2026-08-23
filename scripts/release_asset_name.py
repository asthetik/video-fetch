#!/usr/bin/env python3
"""Release installer filenames aligned with cc-switch style.

Template: Video-Fetch-v{version}-{OS}[-{arch}].{ext}

Rules:
- macOS: never append arch
- Windows: always append arch (-x64 or -arm64)
- Linux: always append x86_64 or arm64
"""

from __future__ import annotations

import argparse


def normalize_version(version: str) -> str:
    v = version.strip()
    if v.startswith("v") or v.startswith("V"):
        v = v[1:]
    if not v:
        raise ValueError("version must not be empty")
    return v


def asset_filename(os_name: str, arch: str, version: str, ext: str) -> str:
    """Return final Release asset basename (no directory)."""
    ver = normalize_version(version)
    ext = ext.lstrip(".")
    if not ext:
        raise ValueError("ext must not be empty")

    if os_name == "macOS":
        stem = f"Video-Fetch-v{ver}-macOS"
    elif os_name == "Windows":
        if arch in {"x64", "x86_64", "amd64"}:
            stem = f"Video-Fetch-v{ver}-Windows-x64"
        elif arch == "arm64":
            stem = f"Video-Fetch-v{ver}-Windows-arm64"
        else:
            raise ValueError(f"unsupported Windows arch: {arch!r}")
    elif os_name == "Linux":
        if arch in {"x86_64", "amd64"}:
            linux_arch = "x86_64"
        elif arch == "arm64":
            linux_arch = "arm64"
        else:
            raise ValueError(f"unsupported Linux arch: {arch!r}")
        stem = f"Video-Fetch-v{ver}-Linux-{linux_arch}"
    else:
        raise ValueError(f"unsupported OS: {os_name!r} (use macOS|Windows|Linux)")

    return f"{stem}.{ext}"


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--os", required=True, dest="os_name", help="macOS|Windows|Linux")
    p.add_argument("--arch", required=True, help="arm64|x64|x86_64")
    p.add_argument("--version", required=True)
    p.add_argument("--ext", required=True)
    args = p.parse_args(argv)
    print(asset_filename(args.os_name, args.arch, args.version, args.ext))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
