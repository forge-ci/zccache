"""Tests for the Azure Artifact Signing release step.

Signing itself needs Azure credentials and only runs in the release workflow;
these tests cover everything around it: the pinned jsign command line, the
structural signature check that fails the step on an unsigned PE, and the
workflow wiring that keeps signing ahead of packaging. PE images are
assembled with ``struct.pack`` so no binary fixture ships.
"""

from __future__ import annotations

import importlib.util
import struct
import sys
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parents[2]


def _load_sign():
    spec = importlib.util.spec_from_file_location(
        "sign_windows_release", ROOT / "ci" / "sign_windows_release.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


sign = _load_sign()

PUBLISHER = "Zachary Vorhies"


def make_pe(cert: bytes | None = None, *, pe32_plus: bool = True) -> bytes:
    """A minimal PE whose security directory points at ``cert`` (if any)."""
    pe_offset = 0x40
    image = bytearray(0x200)
    image[0:2] = b"MZ"
    struct.pack_into("<I", image, 0x3C, pe_offset)
    image[pe_offset : pe_offset + 4] = b"PE\0\0"
    optional = pe_offset + 24
    struct.pack_into("<H", image, optional, 0x20B if pe32_plus else 0x10B)
    if cert is not None:
        directories = optional + (112 if pe32_plus else 96)
        struct.pack_into("<II", image, directories + 4 * 8, len(image), len(cert))
        image += cert
    return bytes(image)


def win_certificate(payload: bytes) -> bytes:
    return struct.pack("<IHH", 8 + len(payload), 0x0200, 0x0002) + payload


SIGNED_PAYLOAD = b"\x30\x82" + PUBLISHER.encode() + sign.RFC3161_TIMESTAMP_OID


def test_unsigned_pe_is_rejected() -> None:
    assert "no Authenticode signature" in sign.signature_problem(make_pe(), PUBLISHER)


@pytest.mark.parametrize("pe32_plus", [True, False])
def test_timestamped_signature_by_publisher_passes(pe32_plus: bool) -> None:
    image = make_pe(win_certificate(SIGNED_PAYLOAD), pe32_plus=pe32_plus)
    assert sign.signature_problem(image, PUBLISHER) is None


def test_wrong_publisher_and_missing_timestamp_are_rejected() -> None:
    other = make_pe(win_certificate(b"CN=Someone Else" + sign.RFC3161_TIMESTAMP_OID))
    assert "publisher" in sign.signature_problem(other, PUBLISHER)
    untimed = make_pe(win_certificate(PUBLISHER.encode()))
    assert "timestamp" in sign.signature_problem(untimed, PUBLISHER)


def test_verify_staging_dir_fails_on_any_unsigned_pyd(tmp_path: Path) -> None:
    (tmp_path / "zccache.exe").write_bytes(make_pe(win_certificate(SIGNED_PAYLOAD)))
    native = tmp_path / "python" / "zccache" / "watcher"
    native.mkdir(parents=True)
    (native / "_native.pyd").write_bytes(make_pe())
    (tmp_path / "zccache.pdb").write_bytes(b"not a PE")
    with pytest.raises(sign.SigningError, match="_native.pyd"):
        sign.verify_staging_dir(tmp_path, PUBLISHER)


def test_verify_requires_at_least_one_pe(tmp_path: Path) -> None:
    with pytest.raises(sign.SigningError, match="no Windows PE"):
        sign.verify_staging_dir(tmp_path, PUBLISHER)


def test_endpoint_and_publisher_normalisation() -> None:
    host = sign.endpoint_host("https://eus.codesigning.azure.net/")
    assert host == "eus.codesigning.azure.net"
    with pytest.raises(sign.SigningError):
        sign.endpoint_host("https://example.com/")
    dn = "CN=Zachary Vorhies, O=Zachary Vorhies, L=Berkeley, C=US"
    assert sign.publisher_common_name(dn) == PUBLISHER
    assert sign.publisher_common_name(PUBLISHER) == PUBLISHER


def test_jsign_command_keeps_the_token_off_argv() -> None:
    config = sign.SigningConfig("eus.codesigning.azure.net", "acct", "prof", PUBLISHER)
    command = sign.jsign_command(Path("jsign.jar"), config, [Path("a.exe")])
    assert command[command.index("--storetype") + 1] == "TRUSTEDSIGNING"
    assert command[command.index("--alias") + 1] == "acct/prof"
    assert command[command.index("--storepass") + 1] == f"env:{sign.TOKEN_ENV}"
    assert (
        command[command.index("--tsaurl") + 1] == "http://timestamp.acs.microsoft.com"
    )
    assert command[command.index("--tsmode") + 1] == "RFC3161"
    assert command[command.index("--alg") + 1] == "SHA-256"
    assert command[-1] == "a.exe"


def test_build_target_signs_before_packaging() -> None:
    action = yaml.safe_load(
        (ROOT / ".github/actions/build-target/action.yml").read_text(encoding="utf-8")
    )
    names = [step.get("name", "") for step in action["runs"]["steps"]]
    login = names.index("Azure login for Authenticode signing")
    signing = names.index("Authenticode-sign staged Windows binaries")
    assert names.index("Stamp release binaries with release marker") < login < signing
    assert signing < names.index("Package standalone archive")


def test_release_build_job_can_mint_oidc_and_requests_signing() -> None:
    workflow = yaml.safe_load(
        (ROOT / ".github/workflows/auto-release.yml").read_text(encoding="utf-8")
    )
    build = workflow["jobs"]["build"]
    assert build["permissions"]["id-token"] == "write"
    step = next(
        s for s in build["steps"] if s.get("uses") == "./.github/actions/build-target"
    )
    assert "pc-windows-msvc" in step["with"]["authenticode_sign"]
    tests = workflow["jobs"]["test-wheels"]["steps"]
    assert any("check-installed" in s.get("run", "") for s in tests)
