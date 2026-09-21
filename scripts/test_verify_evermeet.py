#!/usr/bin/env python3
from __future__ import annotations

import re
import unittest

from fetch_sidecars import FFMPEG_VERSION, load_pins
from verify_evermeet import (
    EVERMEET_FPR,
    fingerprint_from_colons,
    signer_from_status,
)

SUBKEY_FPR = "0123456789ABCDEF0123456789ABCDEF01234567"
OTHER_FPR = "FEDCBA9876543210FEDCBA9876543210FEDCBA98"

# Shape of `gpg --with-colons --import-options show-only --import` on an
# exported public key block: the pub record is followed by its fpr record,
# subkeys after.
COLONS = """\
tru::1:1758418000:0:3:1:5
pub:u:255:22:476C4B611A660874:1264723200:::u:::scESC::::::23::0:
fpr:::::::::20F6EA3E0CFD6B4C53447A73476C4B611A660874:
uid:u::::1264723200::F1E2D3C4B5A6978877665544332211AA00BBCCDD::evermeet.cx <evermeet@gmail.com>::::::::::0:
sub:u:255:22:5A6B7C8D9E0F1A2B:1600000000::::::e::::::23:
fpr:::::::::0123456789ABCDEF0123456789ABCDEF01234567:
"""


def validsig(signer: str, primary: str) -> str:
    return (
        "[GNUPG:] GOODSIG 476C4B611A660874 evermeet.cx <evermeet@gmail.com>\n"
        f"[GNUPG:] VALIDSIG {signer} 2026-09-20 1758326400 0 4 0 1 8 00 {primary}\n"
        "[GNUPG:] TRUST_ULTIMATE 0 pgp\n"
    )


class FingerprintFromColonsTest(unittest.TestCase):
    def test_reads_the_primary_key(self) -> None:
        self.assertEqual(fingerprint_from_colons(COLONS), EVERMEET_FPR)

    def test_evermeet_fingerprint_is_a_full_hex_fingerprint(self) -> None:
        self.assertRegex(EVERMEET_FPR, re.compile(r"^[0-9A-F]{40}$"))

    def test_empty_output_dies(self) -> None:
        with self.assertRaises(SystemExit):
            fingerprint_from_colons("")


class SignerFromStatusTest(unittest.TestCase):
    def test_accepts_a_subkey_of_evermeet(self) -> None:
        self.assertEqual(signer_from_status(validsig(SUBKEY_FPR, EVERMEET_FPR)), SUBKEY_FPR)

    def test_accepts_a_signature_by_the_primary_key_itself(self) -> None:
        self.assertEqual(signer_from_status(validsig(EVERMEET_FPR, EVERMEET_FPR)), EVERMEET_FPR)

    def test_another_key_dies(self) -> None:
        with self.assertRaises(SystemExit):
            signer_from_status(validsig(OTHER_FPR, OTHER_FPR))

    def test_status_without_validsig_dies(self) -> None:
        with self.assertRaises(SystemExit):
            signer_from_status("[GNUPG:] BADSIG 476C4B611A660874 evermeet.cx\n")


class PinsTest(unittest.TestCase):
    def test_the_pinned_version_zip_is_in_the_pin_file(self) -> None:
        downloads = load_pins()["downloads"]
        artifact = f"ffmpeg-{FFMPEG_VERSION}.zip"
        self.assertIn(artifact, downloads, f"{artifact} must be pinned to verify it")


if __name__ == "__main__":
    unittest.main()
