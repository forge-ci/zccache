"""Static contract checks for the Bosn Linux test entrypoint.

The entrypoint is exercised in Bosn on Linux.  Keep this host-safe assertion
so a shell parameter-expansion change cannot reinterpret the literal braces in
the documented usage string before Bosn gets a chance to run it.
"""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "ci/docker/profile/run_bosn_check.sh"


def test_mode_capture_does_not_embed_braced_usage_text() -> None:
    text = SCRIPT.read_text(encoding="utf-8")

    assert 'MODE="${1:-}"' in text
    assert "MODE=${1:?usage:" not in text
    validation = '''case "${MODE}" in
    test)
        ;;
    *)
        echo "usage: run_bosn_check.sh {test}" >&2
        exit 2
        ;;
esac'''
    assert validation in text
    assert text.index(validation) < text.index("seed_soldr_home")


def test_bosn_never_lints() -> None:
    """Lint never runs under bosn; `soldr lint` runs it on the host."""
    text = SCRIPT.read_text(encoding="utf-8")
    assert "clippy" not in text
    assert "cargo fmt" not in text
    bosn = (ROOT / "bosn.toml").read_text(encoding="utf-8")
    assert "[task.lint]" not in bosn
    assert "clippy" not in bosn
    assert "soldr dylint" not in bosn
