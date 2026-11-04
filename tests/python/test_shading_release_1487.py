"""Issue 1487: camera-box deploys the pinned shading release (scripts/lib/shading-release.sh).

Every asset is proved against the pin AND the release's .sha256; the relay and the service must print
the tag's version; there is no fallback to any other build. Served from file://, nothing reaches GitHub.
"""
import hashlib
import shutil
import subprocess
import tarfile
from pathlib import Path

import pytest

from shading_release_fake_1487 import fake_release_env, pin_text

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "shading-release.sh"
RELAY = "bkshading-relay-linux-amd64"
ARM = "bkshading-relay-linux-arm64"
SERVICE = "bkshading-service-linux-amd64.tar.gz"
EXE = "bkshading-service-windows-amd64.exe"


def _lib(body, *args, env=None):
    e = {"PATH": "/usr/bin:/bin"}
    e.update(env or {})
    return subprocess.run(["bash", "-c", f"set -euo pipefail\n. '{LIB}'\n{body}", "harness", *args],
                          capture_output=True, text=True, timeout=60, env=e)


def _script(p, text):
    p.write_text(text)
    p.chmod(0o755)
    return p


def _relay(tmp_path, version="9.9.9-test"):
    return _script(tmp_path / "relay-src", f"#!/bin/sh\necho 'bkshading-relay {version}'\n")


def _service(tmp_path, version="9.9.9-test"):
    d = tmp_path / "svc-src"
    (d / "web").mkdir(parents=True)
    (d / "web" / "index.html").write_text("<html></html>\n")
    _script(d / "bkshading", f"#!/bin/sh\necho 'bkshading {version}'\n")
    tb = tmp_path / "svc.tar.gz"
    with tarfile.open(tb, "w:gz") as t:
        t.add(d / "bkshading", arcname="bkshading-service/bkshading")
        t.add(d / "web", arcname="bkshading-service/web")
    return tb


def test_the_real_pin_names_every_asset():
    r = _lib('shading_pin_tag "$1"; shading_pin_version "$1"; for a in "$SHADING_ASSET_RELAY_AMD64" '
             '"$SHADING_ASSET_RELAY_ARM64" "$SHADING_ASSET_SERVICE_LINUX" "$SHADING_ASSET_SERVICE_WINDOWS"; '
             'do shading_pin_sha256 "$1" "$a"; done', str(REPO))
    assert r.returncode == 0, r.stderr
    lines = r.stdout.split()
    assert lines[0].startswith("v") and lines[1] == lines[0][1:]
    assert len(lines) == 6 and all(len(x) == 64 for x in lines[2:]), lines


def test_the_relay_is_fetched_proved_and_needs_no_network_the_second_time(tmp_path):
    env = fake_release_env(tmp_path, {RELAY: _relay(tmp_path)})
    r = _lib('shading_release_relay "$1" amd64', str(REPO), env=env)
    assert r.returncode == 0, r.stderr
    got = Path(r.stdout.strip())
    assert got == Path(env["SHADING_RELEASE_CACHE"]) / "v9.9.9-test" / RELAY
    assert got.stat().st_mode & 0o111
    (Path(env["SHADING_RELEASE_URL"][len("file://"):]) / "v9.9.9-test" / RELAY).unlink()
    r = _lib('shading_release_relay "$1" amd64', str(REPO), env=env)
    assert r.returncode == 0 and Path(r.stdout.strip()) == got, r.stderr


def test_bytes_the_pin_does_not_name_are_refused_and_not_kept(tmp_path):
    env = fake_release_env(tmp_path, {RELAY: _relay(tmp_path)})
    served = Path(env["SHADING_RELEASE_URL"][len("file://"):]) / "v9.9.9-test" / RELAY
    served.write_text("#!/bin/sh\necho 'bkshading-relay 9.9.9-test'\n# another build\n")
    r = _lib('shading_release_relay "$1" amd64', str(REPO), env=env)
    assert r.returncode == 1, r.stdout
    assert "the pin says" in r.stderr
    assert list((Path(env["SHADING_RELEASE_CACHE"]) / "v9.9.9-test").iterdir()) == [], "nothing unproved stays"


def test_a_sha256_file_that_contradicts_the_pin_is_refused(tmp_path):
    env = fake_release_env(tmp_path, {RELAY: _relay(tmp_path)})
    side = Path(env["SHADING_RELEASE_URL"][len("file://"):]) / "v9.9.9-test" / (RELAY + ".sha256")
    side.write_text("%s  %s\n" % ("0" * 64, RELAY))
    r = _lib('shading_release_fetch "$1" "$2"', str(REPO), RELAY, env=env)
    assert r.returncode == 1 and "its .sha256 says " + "0" * 64 in r.stderr, r.stderr


def test_a_cached_asset_with_other_bytes_is_replaced_by_the_proved_one(tmp_path):
    src = _relay(tmp_path)
    env = fake_release_env(tmp_path, {RELAY: src})
    c = Path(env["SHADING_RELEASE_CACHE"]) / "v9.9.9-test"
    c.mkdir(parents=True)
    (c / RELAY).write_text("stale\n")
    r = _lib('shading_release_fetch "$1" "$2"', str(REPO), RELAY, env=env)
    assert r.returncode == 0, r.stderr
    assert (c / RELAY).read_bytes() == src.read_bytes()


def test_no_release_means_no_relay_never_another_build(tmp_path):
    env = fake_release_env(tmp_path, {RELAY: _relay(tmp_path)})
    shutil.rmtree(Path(env["SHADING_RELEASE_URL"][len("file://"):]) / "v9.9.9-test")
    r = _lib('shading_release_relay "$1" amd64', str(REPO), env=env)
    assert r.returncode == 1 and "never another build" in r.stderr, r.stderr
    assert r.stdout == ""


def test_a_binary_that_says_another_version_is_refused(tmp_path):
    env = fake_release_env(tmp_path, {RELAY: _relay(tmp_path, version="1.0.0")})
    r = _lib('shading_release_relay "$1" amd64', str(REPO), env=env)
    assert r.returncode == 1 and "not 'bkshading-relay 9.9.9-test'" in r.stderr, r.stderr


def test_the_arm64_relay_is_proved_by_sha256_alone(tmp_path):
    arm = tmp_path / "arm"
    arm.write_bytes(b"\x7fELF not runnable here")
    env = fake_release_env(tmp_path, {ARM: arm})
    r = _lib('shading_release_relay "$1" arm64', str(REPO), env=env)
    assert r.returncode == 0, r.stderr
    assert Path(r.stdout.strip()).read_bytes() == arm.read_bytes()


def test_the_service_is_extracted_from_the_proved_tarball(tmp_path):
    env = fake_release_env(tmp_path, {SERVICE: _service(tmp_path)})
    r = _lib('shading_release_service "$1"', str(REPO), env=env)
    assert r.returncode == 0, r.stderr
    d = Path(r.stdout.strip())
    assert (d / "web" / "index.html").is_file() and (d / "bkshading").stat().st_mode & 0o111
    r = _lib('shading_release_service "$1"', str(REPO), env=env)
    assert r.returncode == 0, "a second run over an extracted tree: " + r.stderr


def _chown_logger(tmp_path):
    """A `chown` first on PATH that logs its arguments, then runs the real one."""
    d = tmp_path / "chownbin"
    d.mkdir(exist_ok=True)
    log = tmp_path / "chown.log"
    (d / "chown").write_text('#!/bin/sh\nprintf "%%s\\n" "$*" >> %s\nexec /bin/chown "$@"\n' % log)
    (d / "chown").chmod(0o755)
    return d, log


# 3.11.2026: setup-strih runs as root and extracted the service into the operator-owned staged tree as
# root, so the next deploy's rsync and sweep (the operator) could not replace it (rsync rc 23, the
# deploy stopped). The extracted tree takes the owner of the directory it lands in.
def test_the_extracted_service_takes_the_owner_of_its_directory(tmp_path):
    env = fake_release_env(tmp_path, {SERVICE: _service(tmp_path)})
    d, log = _chown_logger(tmp_path)
    env["PATH"] = "%s:/usr/bin:/bin" % d
    r = _lib('shading_release_service "$1"', str(REPO), env=env)
    assert r.returncode == 0, r.stderr
    placed = r.stdout.strip()
    assert ("-R --reference=%s %s" % (Path(placed).parent, placed)) in log.read_text().splitlines(), \
        log.read_text() if log.exists() else "chown never ran"


def test_a_service_of_another_version_is_refused(tmp_path):
    env = fake_release_env(tmp_path, {SERVICE: _service(tmp_path, version="1.0.0")})
    r = _lib('shading_release_service "$1"', str(REPO), env=env)
    assert r.returncode == 1 and "not 'bkshading 9.9.9-test'" in r.stderr, r.stderr
    assert not (Path(env["SHADING_RELEASE_CACHE"]) / "v9.9.9-test" / "bkshading-service").exists()


def test_a_tarball_without_the_contracts_layout_is_refused(tmp_path):
    src = tmp_path / "x"
    src.write_text("x")
    tb = tmp_path / "bad.tar.gz"
    with tarfile.open(tb, "w:gz") as t:
        t.add(src, arcname="something-else")
    env = fake_release_env(tmp_path, {SERVICE: tb})
    r = _lib('shading_release_service "$1"', str(REPO), env=env)
    assert r.returncode == 1 and "has no bkshading-service/bkshading" in r.stderr, r.stderr


def test_file_ok_passes_only_the_pinned_bytes(tmp_path):
    a = tmp_path / "a"
    a.write_text("pinned")
    b = tmp_path / "b"
    b.write_text("another")
    pin = tmp_path / "pin"
    pin.write_text(pin_text("9.9.9", {EXE: hashlib.sha256(b"pinned").hexdigest()}))
    env = {"SHADING_RELEASE_PIN_FILE": str(pin)}
    assert _lib('shading_release_file_ok "$1" "$2" "$3"', str(REPO), EXE, str(a), env=env).returncode == 0
    r = _lib('shading_release_file_ok "$1" "$2" "$3"', str(REPO), EXE, str(b), env=env)
    assert r.returncode == 1 and "only the pinned release deploys" in r.stderr, r.stderr
    r = _lib('shading_release_file_ok "$1" "$2" "$3"', str(REPO), EXE, str(tmp_path / "none"), env=env)
    assert r.returncode == 1


@pytest.mark.parametrize("text,want", [("SHADING_RELEASE_TAG=v0.2.0\nX=1\n", "0.2.0"), ("", ""), ("X=1\n", "")])
def test_a_pin_is_read_from_another_refs_text(text, want):
    r = _lib('shading_pin_version_of_text "$1"', text)
    assert r.returncode == 0 and r.stdout.strip() == want


def _grade(tmp_path, json_text, want_line):
    f = tmp_path / "v.json"
    f.write_text(json_text)
    bin_ = _script(tmp_path / "bks", "#!/bin/sh\necho 'bkshading 0.0.1'\n")
    r = _lib('ok() { echo "OK $*"; }\nbad() { echo "BAD $*"; }\nstrih_bkshading_version_grade http://x',
             env={"STRIH_BKSHADING_VERSION_JSON_FILE": str(f), "STRIH_BKSHADING_BIN": str(bin_)})
    assert r.returncode == 0, r.stderr
    assert want_line in r.stdout, r.stdout


def test_the_verify_row_grades_the_running_service_then_the_binary(tmp_path):
    pin = next(ln.split("=v")[1] for ln in (REPO / "scripts/shading-release.pin").read_text().splitlines()
               if ln.startswith("SHADING_RELEASE_TAG"))
    _grade(tmp_path, '{"version":"%s"}' % pin, "OK (bkshading-service) version %s" % pin)
    _grade(tmp_path, '{"version":"1.8.6"}', "BAD (bkshading-service) version '1.8.6'")
    _grade(tmp_path, "", "BAD (bkshading-service) version '0.0.1'")
