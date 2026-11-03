"""Issue 1487: a stand-in shading release for the deploy tests (not a test module).

Only the pinned bytes deploy, so a test that hands the deploy a fake binary also hands it a pin that
says those bytes (the lib's SHADING_RELEASE_PIN_FILE seam). `fake_release_env` serves a whole release
from a file:// directory, so nothing here reaches GitHub. `contract()` is the pinned docs/CONTRACT.md.
"""
import hashlib
import re
import tarfile
from pathlib import Path

PINNED = Path(__file__).resolve().parent.parent / "fixtures" / "shading_pinned"
_KEYS = {"bkshading-relay-linux-amd64": "SHADING_RELAY_LINUX_AMD64_SHA256",
         "bkshading-relay-linux-arm64": "SHADING_RELAY_LINUX_ARM64_SHA256",
         "bkshading-service-linux-amd64.tar.gz": "SHADING_SERVICE_LINUX_AMD64_SHA256",
         "bkshading-service-windows-amd64.exe": "SHADING_SERVICE_WINDOWS_AMD64_SHA256"}


def _sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def contract():
    return (PINNED / "CONTRACT.md").read_text(encoding="utf-8")


def service_port():
    """The port the contract's Service section says the service listens on."""
    section = contract().split("## Service", 1)[1]
    return re.search(r"\*\*Listen:\*\* `0\.0\.0\.0:(\d+)`", section).group(1)


def pin_text(version, shas):
    lines = ["SHADING_RELEASE_TAG=v%s" % version]
    lines += ["%s=%s" % (_KEYS[asset], sha) for asset, sha in shas.items()]
    return "\n".join(lines) + "\n"


def pin_for_args(args, version="9.9.9-test"):
    """The env that makes the file after --binary in ARGS the pinned bytes of every asset ({} when
    there is no such file). The pin lands beside that file."""
    if "--binary" not in args:
        return {}
    b = Path(args[args.index("--binary") + 1])
    if not b.is_file():
        return {}
    pin = b.with_name(b.name + ".shading-pin")
    pin.write_text(pin_text(version, {asset: _sha(b) for asset in _KEYS}))
    return {"SHADING_RELEASE_PIN_FILE": str(pin)}


def fake_release_env(tmp_path, assets, version="9.9.9-test"):
    """Publish ASSETS ({asset name: source file}) as a file:// release under TMP_PATH; returns the env
    (pin + download base + cache) that makes the lib use it."""
    base = Path(tmp_path) / "fake-shading-release"
    d = base / ("v%s" % version)
    d.mkdir(parents=True)
    shas = {}
    for asset, src in assets.items():
        (d / asset).write_bytes(Path(src).read_bytes())
        shas[asset] = _sha(src)
        (d / (asset + ".sha256")).write_text("%s  %s\n" % (shas[asset], asset))
    pin = base / "pin"
    pin.write_text(pin_text(version, shas))
    return {"SHADING_RELEASE_PIN_FILE": str(pin), "SHADING_RELEASE_URL": "file://%s" % base,
            "SHADING_RELEASE_CACHE": str(base / "cache")}


def fake_service_env(tmp_path, version="9.9.4"):
    """A service release already in its cache, for the deploy tests that stub curl."""
    d = Path(tmp_path) / "fake-shading-service"
    (d / "web").mkdir(parents=True)
    b = d / "bkshading"
    b.write_text("#!/bin/sh\necho 'bkshading %s'\n" % version)
    b.chmod(0o755)
    cache = d / "cache" / ("v%s" % version)
    cache.mkdir(parents=True)
    tb = cache / "bkshading-service-linux-amd64.tar.gz"
    with tarfile.open(tb, "w:gz") as t:
        t.add(b, arcname="bkshading-service/bkshading")
        t.add(d / "web", arcname="bkshading-service/web")
    pin = d / "pin"
    pin.write_text(pin_text(version, {"bkshading-service-linux-amd64.tar.gz": _sha(tb)}))
    return {"SHADING_RELEASE_PIN_FILE": str(pin), "SHADING_RELEASE_CACHE": str(d / "cache")}
