#!/usr/bin/env python3
"""Detect whether upstream sidecar builds are newer than the ones we pin.

Read-only by design: nothing here edits a pin, downloads an artifact or writes
a file. It asks three small metadata questions and compares the answers with
the pins, so the monthly workflow can decide whether a human should look:

  yt-dlp          GitHub's newest release tag (the download source) against the
                  pin in requirements-sidecars.txt
  Linux/Windows   the newest BtbN autobuild's version token for the same asset
                  variant we pin — nightlies and other branches never enter
                  into it, because BtbN builds one variant per branch head
  macOS           the newest release build id on martin-riedl's index page for
                  macos/arm64, ignoring its nightly entries: those are always
                  newer than the release build and would otherwise report a
                  difference every single month

    python scripts/check_sidecar_upstream.py

No GITHUB_TOKEN is required, but the workflow passes one so the two API calls
do not share the runner's anonymous quota with everything else on that IP.

Exit codes:

  0   every pin is the newest upstream build
  3   something is newer; each difference is printed as `组件 X → Y`
  1   a request failed or a response did not parse (fetch_bytes and die) — the
      workflow fails the job instead of opening a "new version" issue, because
      "upstream is unreachable" must never read as "out of date"
"""

from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from fetch_sidecars import (  # noqa: E402
    FFMPEG_MACOS_BUILD,
    die,
    fetch_bytes,
    load_pins,
    load_ytdlp_version,
)

GITHUB_API = "https://api.github.com"
YT_DLP_REPO = "yt-dlp/yt-dlp"
BTBN_REPO = "BtBN/FFmpeg-Builds"
MARTIN_RIEDL_INDEX = "https://ffmpeg.martin-riedl.de/"

EXIT_CURRENT = 0
EXIT_OUTDATED = 3

# A BtbN asset is `ffmpeg-<version token>-<platform>-<license>-<branch>.<ext>`;
# the token itself contains dashes (`n9.0.2-3-ga5923073bf`), so the variant is
# matched from the right instead.
_BTBN_TAIL = re.compile(r"-(?:linux64|linuxarm64|win64|winarm64)-[\w.-]+$")


def github_headers() -> dict[str, str]:
    """The GitHub API headers, carrying a token when the environment has one."""
    headers = {"Accept": "application/vnd.github+json"}
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return headers


def version_key(version: str) -> tuple[int, ...]:
    """A numeric sort key for a dotted release version."""
    parts = version.split(".")
    if not parts or not all(p.isascii() and p.isdigit() for p in parts):
        raise ValueError(f"unrecognised version {version!r}")
    return tuple(int(p) for p in parts)


def build_timestamp(build_id: str) -> int:
    """The build's unix timestamp — the one field every id carries, release or
    nightly, which is what makes it usable for ordering a mixed listing."""
    stamp, _, version = build_id.partition("_")
    if not stamp.isdigit() or not version:
        raise ValueError(f"unrecognised build id {build_id!r}")
    return int(stamp)


def build_id_key(build_id: str) -> tuple[int, ...]:
    """Sort key for a release build id (`<timestamp>_<version>`).

    Version first, timestamp second: a rebuild of the version we pin (same
    version, newer timestamp) is still a build we do not have. Nightlies carry
    no version to compare, so callers filter them out first.
    """
    _, _, version = build_id.partition("_")
    return (*version_key(version), build_timestamp(build_id))


def is_nightly(build_id: str) -> bool:
    """True for martin-riedl's `N-<commits>-g<hash>` builds."""
    return build_id.partition("_")[2].startswith("N-")


def listed_build_ids(html: str, os_arch: str) -> list[str]:
    """Build ids the index page links for one `<os>/<arch>` target, oldest
    first. The page carries one release build and one nightly per target."""
    pattern = re.compile(rf"/download/{os_arch}/(\d+_[^/\"]+)/")
    return sorted(set(pattern.findall(html)), key=build_timestamp)


def newest_release_build(html: str, os_arch: str) -> str | None:
    """The newest non-nightly build id the index page links for a target."""
    release = [b for b in listed_build_ids(html, os_arch) if not is_nightly(b)]
    return max(release, key=build_id_key) if release else None


def btbn_variant(name: str) -> str:
    """The part of a BtbN asset name identifying the variant, e.g.
    `-linux64-gpl-9.0.tar.xz`. The version token is the only part that moves
    when the same variant is rebuilt, so the tail finds it in any snapshot."""
    match = _BTBN_TAIL.search(name)
    if match is None:
        raise ValueError(f"unrecognised BtbN asset name {name!r}")
    return name[match.start() :]


def btbn_token(name: str) -> str:
    """The version token of a BtbN asset, e.g. `n9.0.2-3-ga5923073bf`."""
    prefix = "ffmpeg-"
    if not name.startswith(prefix):
        raise ValueError(f"unrecognised BtbN asset name {name!r}")
    tail = btbn_variant(name)
    return name[len(prefix) : len(name) - len(tail)]


def newest_autobuild(releases: list[dict]) -> dict:
    """The newest immutable autobuild release.

    The mutable `latest` tag is not one: it is the same content under a name
    that moves, which is exactly why the pins never use it. The API lists
    releases newest first.
    """
    for release in releases:
        if str(release.get("tag_name", "")).startswith("autobuild-"):
            return release
    raise ValueError("no autobuild release found")


def btbn_finding(assets: list[str], pinned_name: str) -> str | None:
    """The difference for one pinned BtbN asset, or None when it is current."""
    tail = btbn_variant(pinned_name)
    pinned_token = btbn_token(pinned_name)
    for name in assets:
        if name.endswith(tail):
            token = btbn_token(name)
            if token == pinned_token:
                return None
            return f"ffmpeg (Linux/Windows) {pinned_token} → {token}"
    return (
        f"ffmpeg (Linux/Windows)：pin 的变体 {tail} 在最新快照里已不存在，"
        "需要确认上游是否换了分支"
    )


def pinned_btbn_names() -> list[str]:
    """The BtbN asset names we pin, one per Linux/Windows target."""
    return [key for key in load_pins()["downloads"] if key.startswith("ffmpeg-n")]


def collect_findings() -> list[str]:
    findings: list[str] = []

    pinned_tag = load_ytdlp_version()
    latest = json.loads(
        fetch_bytes(f"{GITHUB_API}/repos/{YT_DLP_REPO}/releases/latest", github_headers())
    )["tag_name"]
    if latest != pinned_tag:
        findings.append(f"yt-dlp {pinned_tag} → {latest}")

    releases = json.loads(
        fetch_bytes(
            f"{GITHUB_API}/repos/{BTBN_REPO}/releases?per_page=10", github_headers()
        )
    )
    assets = [a["name"] for a in newest_autobuild(releases)["assets"]]
    for name in pinned_btbn_names():
        finding = btbn_finding(assets, name)
        if finding:
            findings.append(finding)

    html = fetch_bytes(MARTIN_RIEDL_INDEX).decode("utf-8", "replace")
    newest = newest_release_build(html, "macos/arm64")
    if newest is None:
        die("martin-riedl's index page listed no macos/arm64 release build")
    if newest != FFMPEG_MACOS_BUILD:
        findings.append(f"ffmpeg (macOS) {FFMPEG_MACOS_BUILD} → {newest}")

    return findings


def pinned_summary() -> str:
    """What the check compares against, so a "nothing newer" log says what was
    actually looked at instead of just asking to be trusted."""
    tokens = ", ".join(sorted({btbn_token(name) for name in pinned_btbn_names()}))
    return (
        f"pin: yt-dlp {load_ytdlp_version()} / "
        f"ffmpeg (Linux/Windows) {tokens} / ffmpeg (macOS) {FFMPEG_MACOS_BUILD}"
    )


def main() -> None:
    print(pinned_summary())
    try:
        findings = collect_findings()
    except (ValueError, KeyError, TypeError) as e:
        die(f"upstream response did not parse: {e}")
    if not findings:
        print("上游 sidecar 无新版本")
        raise SystemExit(EXIT_CURRENT)
    print("上游有更新：")
    for finding in findings:
        print(f"  {finding}")
    raise SystemExit(EXIT_OUTDATED)


if __name__ == "__main__":
    main()
