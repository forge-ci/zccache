#!/usr/bin/env python3
"""Authenticode-sign the Windows release binaries through Azure Artifact Signing.

Both ``*-pc-windows-msvc`` release lanes cross-build on Linux runners, so the
signer is jsign (https://github.com/ebourg/jsign), a pure-Java Authenticode
implementation with a ``TRUSTEDSIGNING`` keystore for Azure Artifact Signing.
The jar is pinned by version and SHA-256 and refused on mismatch.

The workflow runs ``azure/login`` (OIDC, no client secret) first; this script
then asks the Azure CLI for a short-lived code-signing access token and hands
it to jsign through the environment, never on argv.

``sign --staging-dir`` signs every PE (``.exe``/``.dll``/``.pyd``) in the
build-target ``staging/`` tree. It runs after the release-marker stamp and
before the standalone archive is packaged, and the PyPI wheels are assembled
from the same staged bytes, so the archive, ``SHA256SUMS``, ``manifest.json``
and the wheel RECORDs all describe signed bytes. Every staged PE is then
checked structurally (the PE security directory holds a PKCS#7
``WIN_CERTIFICATE`` naming the expected publisher and carrying an RFC 3161
timestamp); any unsigned PE fails the step.

``check-installed`` runs on native Windows after a wheel install and requires
``Get-AuthenticodeSignature`` to report ``Valid`` for every installed PE.

Usage (CI):
    python ci/sign_windows_release.py sign --staging-dir staging
    python ci/sign_windows_release.py verify --staging-dir staging
    python ci/sign_windows_release.py check-installed
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import os
import shutil
import struct
import subprocess
import sys
import sysconfig
import urllib.request
from dataclasses import dataclass
from pathlib import Path


JSIGN_VERSION = "7.6"
JSIGN_URL = f"https://github.com/ebourg/jsign/releases/download/{JSIGN_VERSION}/jsign-{JSIGN_VERSION}.jar"
JSIGN_SHA256 = "6cf58aa080e7ff82656f320cff8d4a3bff30408df8766806dcba3740dd839fa5"
DEFAULT_JSIGN_DIR = Path("target/jsign")

TIMESTAMP_URL = "http://timestamp.acs.microsoft.com"
CODESIGNING_RESOURCE = "https://codesigning.azure.net"
TOKEN_ENV = "ZCCACHE_AZURE_SIGNING_TOKEN"

PE_SUFFIXES = (".exe", ".dll", ".pyd")

# WIN_CERTIFICATE: wRevision 0x0200, wCertificateType WIN_CERT_TYPE_PKCS_SIGNED_DATA.
WIN_CERT_REVISION_2 = 0x0200
WIN_CERT_TYPE_PKCS_SIGNED_DATA = 0x0002
# DER of OID 1.3.6.1.4.1.311.3.3.1 (Microsoft RFC 3161 timestamp countersignature).
RFC3161_TIMESTAMP_OID = bytes.fromhex("060a2b060104018237030301")
SECURITY_DIRECTORY_INDEX = 4


class SigningError(RuntimeError):
    """A signing precondition or post-signing check failed."""


@dataclass(frozen=True)
class SigningConfig:
    """Azure Artifact Signing coordinates, read from the job environment."""

    endpoint_host: str
    account: str
    profile: str
    publisher: str


def endpoint_host(endpoint: str) -> str:
    """``https://eus.codesigning.azure.net/`` -> ``eus.codesigning.azure.net``."""
    host = endpoint.strip().removeprefix("https://").removeprefix("http://")
    host = host.split("/", 1)[0]
    if not host.endswith(".codesigning.azure.net"):
        raise SigningError(f"unexpected Artifact Signing endpoint: {endpoint!r}")
    return host


def _required_env(env: dict[str, str], name: str) -> str:
    value = env.get(name, "").strip()
    if not value:
        raise SigningError(f"{name} is not set (expected an org Actions secret)")
    return value


def publisher_common_name(publisher: str) -> str:
    """Accept either a bare name or a full subject DN (``CN=..., O=...``)."""
    for part in publisher.split(","):
        key, _, value = part.strip().partition("=")
        if key.strip().upper() == "CN" and value.strip():
            return value.strip().strip('"')
    return publisher.strip()


def config_from_env(env: dict[str, str]) -> SigningConfig:
    return SigningConfig(
        endpoint_host=endpoint_host(_required_env(env, "AZURE_SIGNING_ENDPOINT")),
        account=_required_env(env, "AZURE_SIGNING_ACCOUNT"),
        profile=_required_env(env, "AZURE_SIGNING_PROFILE"),
        publisher=publisher_common_name(_required_env(env, "AZURE_SIGN_PUBLISHER")),
    )


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch_jsign(dest_dir: Path) -> Path:
    """Download the pinned jsign jar once and verify its SHA-256."""
    jar = dest_dir / f"jsign-{JSIGN_VERSION}.jar"
    if jar.is_file() and sha256_file(jar) == JSIGN_SHA256:
        return jar
    dest_dir.mkdir(parents=True, exist_ok=True)
    partial = jar.with_suffix(".partial")
    with urllib.request.urlopen(JSIGN_URL, timeout=120) as response:
        with partial.open("wb") as out:
            shutil.copyfileobj(response, out)
    actual = sha256_file(partial)
    if actual != JSIGN_SHA256:
        partial.unlink()
        raise SigningError(
            f"jsign {JSIGN_VERSION} sha256 mismatch: got {actual}, pinned {JSIGN_SHA256}"
        )
    partial.replace(jar)
    print(f"jsign {JSIGN_VERSION}: sha256 verified ({JSIGN_SHA256})")
    return jar


def access_token() -> str:
    """Mint an Artifact Signing token from the ``azure/login`` session."""
    completed = subprocess.run(
        [
            "az",
            "account",
            "get-access-token",
            "--resource",
            CODESIGNING_RESOURCE,
            "--query",
            "accessToken",
            "-o",
            "tsv",
        ],
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    token = completed.stdout.strip()
    if not token:
        raise SigningError("az returned an empty code-signing access token")
    if os.environ.get("GITHUB_ACTIONS") == "true":
        print(f"::add-mask::{token}", flush=True)
    return token


def jsign_command(jar: Path, config: SigningConfig, files: list[Path]) -> list[str]:
    return [
        "java",
        "-jar",
        str(jar),
        "--storetype",
        "TRUSTEDSIGNING",
        "--keystore",
        config.endpoint_host,
        "--storepass",
        f"env:{TOKEN_ENV}",
        "--alias",
        f"{config.account}/{config.profile}",
        "--alg",
        "SHA-256",
        "--tsaurl",
        TIMESTAMP_URL,
        "--tsmode",
        "RFC3161",
        "--replace",
        *[str(path) for path in files],
    ]


def is_pe_name(name: str) -> bool:
    return name.lower().endswith(PE_SUFFIXES)


def pe_files(directory: Path) -> list[Path]:
    return sorted(p for p in directory.rglob("*") if p.is_file() and is_pe_name(p.name))


def authenticode_blob(data: bytes) -> bytes | None:
    """Return the PE's WIN_CERTIFICATE table, or None when it has none."""
    if data[:2] != b"MZ" or len(data) < 0x40:
        raise SigningError("not a PE image (missing MZ header)")
    (pe_offset,) = struct.unpack_from("<I", data, 0x3C)
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise SigningError("not a PE image (missing PE signature)")
    optional = pe_offset + 24
    (magic,) = struct.unpack_from("<H", data, optional)
    directories = {0x10B: optional + 96, 0x20B: optional + 112}.get(magic)
    if directories is None:
        raise SigningError(f"unknown PE optional-header magic {magic:#x}")
    entry = directories + SECURITY_DIRECTORY_INDEX * 8
    offset, size = struct.unpack_from("<II", data, entry)
    if offset == 0 or size == 0:
        return None
    return data[offset : offset + size]


def signature_problem(data: bytes, publisher: str) -> str | None:
    """Describe why ``data`` is not a timestamped signature by ``publisher``."""
    blob = authenticode_blob(data)
    if blob is None or len(blob) < 8:
        return "no Authenticode signature (empty PE security directory)"
    _length, revision, cert_type = struct.unpack_from("<IHH", blob, 0)
    if revision != WIN_CERT_REVISION_2 or cert_type != WIN_CERT_TYPE_PKCS_SIGNED_DATA:
        return f"unexpected WIN_CERTIFICATE revision/type {revision:#x}/{cert_type:#x}"
    if publisher.encode("utf-8") not in blob:
        return f"signature does not name publisher {publisher!r}"
    if RFC3161_TIMESTAMP_OID not in blob:
        return "signature carries no RFC 3161 timestamp"
    return None


def verify_staging_dir(staging_dir: Path, publisher: str) -> None:
    files = pe_files(staging_dir)
    if not files:
        raise SigningError(f"no Windows PE files found under {staging_dir}")
    failures = []
    for path in files:
        problem = signature_problem(path.read_bytes(), publisher)
        if problem:
            failures.append(f"  {path}: {problem}")
        else:
            print(f"signed: {path}")
    if failures:
        raise SigningError("unsigned Windows release binaries:\n" + "\n".join(failures))


def sign_staging_dir(jar: Path, config: SigningConfig, staging_dir: Path) -> None:
    files = pe_files(staging_dir)
    if not files:
        raise SigningError(f"no Windows PE files found under {staging_dir}")
    env = dict(os.environ)
    env[TOKEN_ENV] = access_token()
    print(f"jsign: signing {len(files)} file(s) with {config.account}/{config.profile}")
    subprocess.run(jsign_command(jar, config, files), env=env, check=True)


def installed_pe_files() -> list[Path]:
    """Every PE the installed ``zccache`` wheel placed on this machine."""
    spec = importlib.util.find_spec("zccache")
    if spec is None or not spec.submodule_search_locations:
        raise SigningError("the zccache package is not installed")
    files: list[Path] = []
    for location in spec.submodule_search_locations:
        files.extend(pe_files(Path(location)))
    scripts = Path(sysconfig.get_path("scripts"))
    for name in ("zccache.exe", "zccache-fp.exe"):
        path = scripts / name
        if not path.is_file():
            raise SigningError(f"installed wheel script missing: {path}")
        files.append(path)
    return files


def authenticode_status(binary: Path) -> str:
    """Windows' own verdict on a PE's signature (chain, timestamp, hash)."""
    literal = str(binary.resolve()).replace("'", "''")
    # A pwsh 7 step exports its own PSModulePath; Windows PowerShell 5.1 then
    # cannot autoload Microsoft.PowerShell.Security. Let 5.1 use its default.
    env = {k: v for k, v in os.environ.items() if k.upper() != "PSMODULEPATH"}
    completed = subprocess.run(
        [
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            f"(Get-AuthenticodeSignature -LiteralPath '{literal}').Status",
        ],
        stdout=subprocess.PIPE,
        text=True,
        check=True,
        env=env,
    )
    return completed.stdout.strip()


def check_installed() -> None:
    files = installed_pe_files()
    failures = [
        f"  {path}: {status}"
        for path in files
        if (status := authenticode_status(path)) != "Valid"
    ]
    if failures:
        raise SigningError(
            "installed Windows binaries without a Valid Authenticode signature:\n"
            + "\n".join(failures)
        )
    print(f"Authenticode Valid: {len(files)} installed Windows PE file(s)")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=("sign", "verify", "check-installed"))
    parser.add_argument("--staging-dir", type=Path)
    parser.add_argument("--jsign-dir", type=Path, default=DEFAULT_JSIGN_DIR)
    args = parser.parse_args(argv)
    if args.mode != "check-installed" and args.staging_dir is None:
        parser.error(f"{args.mode} requires --staging-dir")

    try:
        if args.mode == "check-installed":
            check_installed()
            return 0
        config = config_from_env(dict(os.environ))
        if args.mode == "sign":
            sign_staging_dir(fetch_jsign(args.jsign_dir), config, args.staging_dir)
        verify_staging_dir(args.staging_dir, config.publisher)
    except SigningError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
