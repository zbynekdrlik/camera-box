"""Issue 1487: a stand-in intercom release for the deploy tests (not a test module).

The deploy's tree step runs strih_intercom_release_ensure. These tests stub curl and must never reach
GitHub, so they hand it a local tarball and a pin that matches it (the lib's two seams).
"""
import hashlib
import tarfile
from pathlib import Path

PINNED = Path(__file__).resolve().parent.parent / "fixtures" / "intercom_pinned"
FILES = ("intercom.strih-snv.toml", "intercom.strih-pp.toml", "intercom.stream-snv.toml",
         "intercom.stream-pp.toml", "intercom-hub.service")


def fake_release_env(tmp_path, version="9.9.3"):
    """Write a bundle + its pin under TMP_PATH; returns the env that makes the lib use them."""
    d = Path(tmp_path) / "fake-intercom-release"
    d.mkdir()
    hub = d / "intercom-hub"
    hub.write_text("#!/bin/sh\necho 'intercom-hub %s'\n" % version)
    hub.chmod(0o755)
    tb = d / "bundle.tar.gz"
    with tarfile.open(tb, "w:gz") as t:
        t.add(hub, arcname="intercom-hub")
        for f in FILES:
            t.add(PINNED / f, arcname=f)
    pin = d / "pin"
    pin.write_text("INTERCOM_RELEASE_TAG=v%s\nINTERCOM_RELEASE_SHA256=%s\n"
                   % (version, hashlib.sha256(tb.read_bytes()).hexdigest()))
    return {"STRIH_INTERCOM_PIN_FILE": str(pin), "STRIH_INTERCOM_TARBALL_SRC": str(tb)}
