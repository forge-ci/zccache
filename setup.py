"""Dynamic version: reads from workspace Cargo.toml so version is defined once."""

import re
import sys
from pathlib import Path

from setuptools import setup

sys.path.insert(0, str(Path(__file__).resolve().parent))
from ci.source_package import setup_options

_cargo = (Path(__file__).parent / "Cargo.toml").read_text()
_match = re.search(r'\[workspace\.package\]\s*\nversion\s*=\s*"([^"]+)"', _cargo)
assert _match, "Could not find [workspace.package] version in Cargo.toml"

setup(version=_match.group(1), **setup_options())
