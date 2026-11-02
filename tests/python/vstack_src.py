"""Issue 1487: the pinned vstack source, read from cargo's git checkout of the tag.

The same lookup as tests/support/vstack.rs. A cargo build or `cargo fetch` puts the checkout on disk
(ci.yml's python-tests job runs `cargo fetch`); a missing one fails loudly, it never skips.
"""
import os
from pathlib import Path

_REPO = Path(__file__).resolve().parents[2]


def pin(key):
    """The value of KEY in scripts/vstack-release.pin."""
    for line in (_REPO / "scripts" / "vstack-release.pin").read_text(encoding="utf-8").splitlines():
        if line.startswith(key + "="):
            return line[len(key) + 1 :]
    raise AssertionError(f"no {key} in scripts/vstack-release.pin")


def root():
    """`<cargo home>/git/checkouts/vstack-<hash>/<short commit>/` of the pinned commit."""
    commit = pin("VSTACK_RELEASE_COMMIT")
    home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
    checkouts = home / "git" / "checkouts"
    for db in checkouts.glob("vstack-*"):
        for co in db.iterdir():
            if commit.startswith(co.name) and (co / "rig" / "core" / "Cargo.toml").exists():
                return co
    raise AssertionError(f"no checkout of vstack {commit} under {checkouts}: run `cargo fetch`")


def read(rel):
    """The text of a file inside the pinned tag."""
    return (root() / rel).read_text(encoding="utf-8")
