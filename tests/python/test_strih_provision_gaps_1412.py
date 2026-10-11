"""issue 1412: provisioning gaps found live on strih-pp / stream-pp (11.10.2026).

Each test drives the REAL lib functions (sourced under the callers' `set -euo pipefail`), values passed
as positional arguments, never spliced into the script text. Tier-0: pytest + bash, no root, no rig.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
LIB = REPO / "scripts" / "lib" / "strih-provision.sh"


def _bash(script, *args, env=None, cwd=None):
    full = 'set -euo pipefail\n. "$LIB"\n' + script
    e = dict(os.environ, LIB=str(LIB))
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", full, "harness", *map(str, args)], capture_output=True, text=True,
                          env=e, cwd=cwd, timeout=120)


def _sections(text):
    """systemd-style sections -> {section: [key=value lines]} (comments and blanks dropped)."""
    out, cur = {}, None
    for line in text.splitlines():
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        if s.startswith("[") and s.endswith("]"):
            cur = s[1:-1]
            out.setdefault(cur, [])
            continue
        out.setdefault(cur, []).append(s)
    return out


# --- bug 4: the local-audio drop-in's After= must sit in [Unit] (systemd ignores it in [Service]) ---

def test_the_local_audio_dropin_orders_after_the_user_manager_in_its_unit_section():
    r = _bash('strih_intercom_audio_dropin "$1" "$2"', "newlevel", 1000)
    assert r.returncode == 0, r.stderr
    sec = _sections(r.stdout)
    assert "After=user@1000.service" in sec.get("Unit", []), sec
    assert not any(k.startswith("After=") for k in sec.get("Service", [])), sec
    # the Service half is unchanged
    for want in ("DynamicUser=no", "User=newlevel", "Environment=XDG_RUNTIME_DIR=/run/user/1000"):
        assert want in sec["Service"], (want, sec)


def test_no_unit_only_key_sits_in_the_service_section_of_the_dropin():
    r = _bash('strih_intercom_audio_dropin "$1" "$2"', "newlevel", 1000)
    unit_only = ("After=", "Before=", "Wants=", "Requires=", "BindsTo=", "PartOf=", "Description=")
    bad = [k for k in _sections(r.stdout).get("Service", []) if k.startswith(unit_only)]
    assert bad == [], bad


# --- bug 1: step 4 must never rewrite a file a running OBS maps (cp -a truncates in place) ---------

def _stubs(tmp_path):
    """chown / ldconfig as logged no-ops, install without -o/-g (the test is not root)."""
    d = tmp_path / "stubs"
    d.mkdir(exist_ok=True)
    log = tmp_path / "stub.log"
    (d / "chown").write_text('#!/bin/bash\necho "chown $*" >> "%s"\nexit 0\n' % log)
    (d / "ldconfig").write_text('#!/bin/bash\necho "ldconfig $*" >> "%s"\nexit 0\n' % log)
    (d / "install").write_text(
        '#!/bin/bash\nargs=()\nwhile [ $# -gt 0 ]; do\n  case "$1" in -o|-g) shift 2 ;; '
        '*) args+=("$1"); shift ;; esac\ndone\nexec /usr/bin/install "${args[@]}"\n')
    for f in d.iterdir():
        f.chmod(0o755)
    return {"PATH": "%s:%s" % (d, os.environ["PATH"])}


def _bundle(root, files, links=None, modes=None):
    """A staged bundle: {relpath: bytes} under ROOT, plus symlinks and explicit modes."""
    for rel, data in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        p.chmod((modes or {}).get(rel, 0o644))
    for rel, target in (links or {}).items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.symlink_to(target)
    return root


def _prefix_install(tmp_path, bundle, env):
    pre = tmp_path / "usr"
    (pre / "bin").mkdir(parents=True, exist_ok=True)   # /usr/bin always exists on a box
    return _bash('strih_install_bundle_prefix "$1" "$2/lib" "$2/bin" "$2/share"', bundle, pre, env=env), pre


BASE = {
    "lib/x86_64-linux-gnu/libobs.so.30": b"libobs v1",
    "lib/x86_64-linux-gnu/obs-plugins/distroav.so": b"distroav v1",
    "lib/x86_64-linux-gnu/obs-plugins/obs-browser-page": Path("/bin/sleep").read_bytes(),
    "bin/obs": b"obs v1",
    "bin/obs-ffmpeg-mux": b"mux v1",
    "share/obs/libobs/default.effect": b"effect v1",
}
LINKS = {"lib/x86_64-linux-gnu/libobs.so": "libobs.so.30"}
PAGE = "lib/x86_64-linux-gnu/obs-plugins/obs-browser-page"


def test_a_changed_library_lands_on_a_new_inode_and_the_mapped_one_keeps_its_bytes(tmp_path):
    env = _stubs(tmp_path)
    r, pre = _prefix_install(tmp_path, _bundle(tmp_path / "b1", BASE, LINKS), env)
    assert r.returncode == 0, r.stderr
    lib = pre / "lib" / "libobs.so.30"
    held = os.open(lib, os.O_RDONLY)   # a running OBS keeps the inode it mapped open
    ino = lib.stat().st_ino
    v2 = dict(BASE, **{"lib/x86_64-linux-gnu/libobs.so.30": b"libobs v2 -- a different build"})
    r, pre = _prefix_install(tmp_path, _bundle(tmp_path / "b2", v2, LINKS), env)
    assert r.returncode == 0, r.stderr
    assert lib.read_bytes() == b"libobs v2 -- a different build"
    assert lib.stat().st_ino != ino, "the new library must be a NEW inode (temp + rename), never cp -a in place"
    os.lseek(held, 0, 0)
    got = os.read(held, 100)
    os.close(held)
    assert got == b"libobs v1", "the running process's open copy was rewritten under it"


def test_an_unchanged_bundle_writes_no_file(tmp_path):
    env = _stubs(tmp_path)
    b = _bundle(tmp_path / "b1", BASE, LINKS)
    r, pre = _prefix_install(tmp_path, b, env)
    assert r.returncode == 0, r.stderr
    before = {p: (p.lstat().st_ino, p.lstat().st_mtime_ns) for p in (pre / "lib").rglob("*")}
    r, pre = _prefix_install(tmp_path, b, env)
    assert r.returncode == 0, r.stderr
    after = {p: (p.lstat().st_ino, p.lstat().st_mtime_ns) for p in (pre / "lib").rglob("*")}
    assert after == before
    assert "unchanged" in r.stdout


def test_a_helper_executing_from_the_prefix_does_not_fail_the_install(tmp_path):
    """The live `Text file busy`: a CEF obs-browser-page runs while step 4 replaces it."""
    env = _stubs(tmp_path)
    r, pre = _prefix_install(tmp_path, _bundle(tmp_path / "b1", BASE, LINKS, {PAGE: 0o755}), env)
    assert r.returncode == 0, r.stderr
    page = pre / "lib" / "obs-plugins" / "obs-browser-page"
    running = subprocess.Popen([str(page), "30"])   # an ELF: the kernel refuses writes to it (ETXTBSY)
    try:
        v2 = dict(BASE, **{PAGE: Path("/bin/true").read_bytes()})
        r, pre = _prefix_install(tmp_path, _bundle(tmp_path / "b2", v2, LINKS, {PAGE: 0o755}), env)
        assert r.returncode == 0, r.stderr
        assert page.read_bytes() == Path("/bin/true").read_bytes()
    finally:
        running.kill()
        running.wait()


def test_symlinks_and_share_data_are_installed(tmp_path):
    env = _stubs(tmp_path)
    r, pre = _prefix_install(tmp_path, _bundle(tmp_path / "b1", BASE, LINKS), env)
    assert r.returncode == 0, r.stderr
    link = pre / "lib" / "libobs.so"
    assert link.is_symlink() and os.readlink(link) == "libobs.so.30"
    assert (pre / "share" / "obs" / "libobs" / "default.effect").read_bytes() == b"effect v1"
    assert (pre / "bin" / "obs").read_bytes() == b"obs v1"
    assert not [p for p in pre.rglob("*") if ".cb-new." in p.name], "no temp sibling may be left behind"


# --- bug 2: an artifact unzipped without modes left obs-browser-page 0664; CEF crash-looped OBS -------

def test_a_mode_less_obs_browser_page_is_installed_executable(tmp_path):
    env = _stubs(tmp_path)
    r, pre = _prefix_install(tmp_path, _bundle(tmp_path / "b1", BASE, LINKS, {PAGE: 0o664}), env)
    assert r.returncode == 0, r.stderr
    mode = (pre / "lib" / "obs-plugins" / "obs-browser-page").stat().st_mode & 0o7777
    assert mode == 0o755, oct(mode)


def test_an_unchanged_but_non_executable_installed_helper_is_fixed(tmp_path):
    """The live box after the bad unzip: the installed copy is byte-identical but 0664."""
    env = _stubs(tmp_path)
    b = _bundle(tmp_path / "b1", BASE, LINKS, {PAGE: 0o664})
    pre = tmp_path / "usr"
    page = pre / "lib" / "obs-plugins" / "obs-browser-page"
    page.parent.mkdir(parents=True)
    page.write_bytes(BASE[PAGE])
    page.chmod(0o664)
    r, pre = _prefix_install(tmp_path, b, env)
    assert r.returncode == 0, r.stderr
    assert page.stat().st_mode & 0o7777 == 0o755


def _grade(libdir, browser):
    return _bash('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nstrih_bundle_exec_grade "$1" "$2"',
                 libdir, browser)


def test_verify_fails_a_non_executable_obs_browser_page(tmp_path):
    page = tmp_path / "obs-plugins" / "obs-browser-page"
    page.parent.mkdir()
    page.write_bytes(b"x")
    page.chmod(0o664)
    r = _grade(tmp_path, 1)
    assert r.returncode == 0, r.stderr
    assert r.stdout.startswith("FAIL") and "obs-browser-page" in r.stdout and "664" in r.stdout, r.stdout
    page.chmod(0o755)
    assert _grade(tmp_path, 1).stdout.startswith("PASS")


def test_verify_fails_a_missing_helper_only_on_a_browser_bundle(tmp_path):
    assert _grade(tmp_path, 1).stdout.startswith("FAIL")
    assert _grade(tmp_path, 0).stdout.startswith("PASS")


def test_verify_strih_runs_the_executables_row_and_setup_lists_the_helper():
    v = (REPO / "scripts" / "verify-strih.sh").read_text()
    assert 'strih_bundle_exec_grade "' in v
    lib = (REPO / "scripts" / "lib" / "strih-bundle-install.sh").read_text()
    assert "obs-plugins/obs-browser-page" in lib


def test_the_prefix_install_never_uses_cp_a_into_the_prefix():
    body = LIB.read_text().split("strih_install_bundle_prefix() {", 1)[1].split("\n}\n", 1)[0]
    assert "cp -a" not in body
