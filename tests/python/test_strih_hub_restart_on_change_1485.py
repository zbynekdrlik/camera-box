"""Issue 1485 (4.11.2026, the in-process PTP switch): the strih-snv deploy installed hub v1.9.13 (binary,
unit, its port-319 capability) through setup-strih, but the RUNNING intercom-hub stayed the old process:
step 13 only `daemon-reload`ed and the `13-hub-live` block starts a STOPPED hub only. Nothing refused it.

Pinned here (design comment 6634248706):
  * `strih_intercom_hub_install_fingerprint`: one `<sha256|absent>  <path>` line per file the hub loads at
    its start (binary, unit, every drop-in, the routing TOML);
  * `strih_intercom_hub_restart_if_changed BEFORE`: a running hub whose fingerprint changed is
    `try-restart`ed and probed for a crash loop; an unchanged one is never touched (the issue-1418 rule);
    a stopped one is never started here; no BEFORE = nothing restarted;
  * setup-strih takes the fingerprint before step 12/13 write anything and calls the restart in the
    `13-hub-live` block, after step 14 (the LoadCredential secret), for a running hub only;
  * the deploy read-back (`strih-snv-deploy.sh`) reads the running hub's /api/version and its verdict FAILs
    a hub that is not the intercom pin's version on a box that runs a hub.
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
HUB_UNIT = SCRIPTS / "lib" / "strih-intercom-hub-unit.sh"
DEPLOY = SCRIPTS / "lib" / "strih-snv-deploy.sh"

SYSTEMCTL = r'''#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_DIR/systemctl.log"
case "$1" in
  is-active) [ "${FAKE_ACTIVE:-1}" = 1 ] && exit 0; exit 3 ;;
  try-restart) exit "${FAKE_RESTART_RC:-0}" ;;
  show) printf 'ActiveState=%s\nSubState=%s\nNRestarts=0\nResult=success\n' "${FAKE_STATE:-active}" "${FAKE_SUB:-running}" ;;
  *) exit 0 ;;
esac
'''


def _box(tmp):
    b = tmp / "box"
    (b / "dropins").mkdir(parents=True)
    (b / "hub").write_text("hub 1.9.11\n")
    (b / "unit").write_text("[Service]\nExecStart=/usr/local/bin/intercom-hub\n")
    (b / "toml").write_text("[hub]\n")
    (b / "dropins" / "10-local-audio.conf").write_text("[Service]\nUser=newlevel\n")
    (b / "dropins" / "ndi-discovery.conf").write_text("[Service]\nEnvironment=NDI_CONFIG_DIR=/etc/ndi\n")
    fake = tmp / "fakebin"
    fake.mkdir()
    (fake / "systemctl").write_text(SYSTEMCTL)
    (fake / "systemctl").chmod(0o755)
    return b


def _run(tmp, body, *args, env=None):
    b = tmp / "box"
    e = dict(os.environ, FAKE_DIR=str(tmp), PATH="%s:%s" % (tmp / "fakebin", os.environ["PATH"]),
             STRIH_HUB_LOOP_WINDOW_S="0", STRIH_HUB_INVOCATION_ID="inv-1",
             STRIH_HUB_LOADED_FILE=str(tmp / "state" / "intercom-hub.loaded"))
    e.update(env or {})
    files = [str(b / "hub"), str(b / "unit"), str(b / "dropins"), str(b / "toml")]
    script = ('set -euo pipefail\n. "%s"\nFP=(%s)\n%s'
              % (HUB_UNIT, " ".join('"%s"' % f for f in files), body))
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _calls(tmp):
    p = tmp / "systemctl.log"
    return p.read_text() if p.exists() else ""


def _changed_run(tmp, mutate, env=None):
    _box(tmp)
    r = _run(tmp, 'B="$(strih_intercom_hub_install_fingerprint "${FP[@]}")"\nprintf "%s" "$B" > "$1"', tmp / "before")
    assert r.returncode == 0, r.stderr
    mutate(tmp / "box")
    return _run(tmp, 'strih_intercom_hub_restart_if_changed "$(cat "$1")" "${FP[@]}"\necho "RC=$?"',
                tmp / "before", env=env)


def test_the_fingerprint_names_every_file_the_hub_loads(tmp_path):
    _box(tmp_path)
    (tmp_path / "box" / "toml").unlink()
    r = _run(tmp_path, 'strih_intercom_hub_install_fingerprint "${FP[@]}"')
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert len(lines) == 5, r.stdout
    assert any(l.startswith("absent  ") and l.endswith("/toml") for l in lines), r.stdout
    assert any(l.endswith("/dropins/10-local-audio.conf") and len(l.split()[0]) == 64 for l in lines), r.stdout


def test_an_unchanged_running_hub_is_never_restarted(tmp_path):
    r = _changed_run(tmp_path, lambda b: None)
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert "try-restart" not in _calls(tmp_path)
    assert "unchanged" in r.stdout, r.stdout


def test_a_new_binary_restarts_the_running_hub(tmp_path):
    r = _changed_run(tmp_path, lambda b: (b / "hub").write_text("hub 1.9.13\n"))
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert "try-restart intercom-hub" in _calls(tmp_path)
    assert "restarted" in r.stdout and "/hub" in r.stdout, r.stdout


def test_a_changed_unit_a_new_dropin_or_a_removed_dropin_restarts_it(tmp_path):
    for i, mutate in enumerate((lambda b: (b / "unit").write_text("[Service]\nAmbientCapabilities=CAP_NET_BIND_SERVICE\n"),
                                lambda b: (b / "dropins" / "20-x.conf").write_text("[Service]\n"),
                                lambda b: (b / "dropins" / "ndi-discovery.conf").unlink(),
                                lambda b: (b / "toml").write_text("[hub]\nx = 1\n"))):
        t = tmp_path / str(i)
        t.mkdir()
        r = _changed_run(t, mutate)
        assert "RC=0" in r.stdout, r.stdout + r.stderr
        assert "try-restart intercom-hub" in _calls(t), (i, r.stdout)


def test_an_unchanged_run_records_what_the_running_hub_runs(tmp_path):
    _changed_run(tmp_path, lambda b: None)
    rec = (tmp_path / "state" / "intercom-hub.loaded").read_text().splitlines()
    assert rec[0] == "invocation=inv-1" and len(rec) == 6, rec


def test_a_failed_earlier_run_still_restarts_the_hub_on_the_next_run(tmp_path):
    """Review round 1: run N wrote the new files and failed before the restart; run N+1 sees no change of
    its own, but the record of what the running invocation was started on differs."""
    _box(tmp_path)
    r = _run(tmp_path, 'strih_intercom_hub_record_loaded "${FP[@]}"')
    assert r.returncode == 0, r.stderr
    (tmp_path / "box" / "hub").write_text("hub 1.9.13\n")   # the failed run's install
    r = _run(tmp_path, 'B="$(strih_intercom_hub_install_fingerprint "${FP[@]}")"\n'
                       'strih_intercom_hub_restart_if_changed "$B" "${FP[@]}"\necho "RC=$?"')
    assert "RC=0" in r.stdout and "try-restart intercom-hub" in _calls(tmp_path), r.stdout + r.stderr


def test_a_record_of_another_invocation_is_not_the_baseline(tmp_path):
    """A hub restarted outside setup loaded the files on disk at its start: the record is stale then."""
    _box(tmp_path)
    _run(tmp_path, 'strih_intercom_hub_record_loaded "${FP[@]}"', env={"STRIH_HUB_INVOCATION_ID": "inv-0"})
    (tmp_path / "box" / "hub").write_text("hub 1.9.13\n")
    r = _run(tmp_path, 'B="$(strih_intercom_hub_install_fingerprint "${FP[@]}")"\n'
                       'strih_intercom_hub_restart_if_changed "$B" "${FP[@]}"\necho "RC=$?"')
    assert "RC=0" in r.stdout and "try-restart" not in _calls(tmp_path), r.stdout + r.stderr


def test_the_fingerprint_survives_the_callers_failglob_and_nullglob(tmp_path):
    _box(tmp_path)
    for f in (tmp_path / "box" / "dropins").iterdir():
        f.unlink()
    for opt in ("failglob", "nullglob"):
        r = _run(tmp_path, 'shopt -s %s\nstrih_intercom_hub_install_fingerprint "${FP[@]}"\necho END' % opt)
        assert r.returncode == 0 and r.stdout.strip().endswith("END"), (opt, r.stdout, r.stderr)
        assert len(r.stdout.splitlines()) == 4, (opt, r.stdout)


def test_the_fingerprint_covers_every_toml_beside_the_routing_file(tmp_path):
    _box(tmp_path)
    etc = tmp_path / "etc"
    etc.mkdir()
    (etc / "intercom.toml").write_text("[hub]\n")
    (etc / "turn.toml").write_text("key = 1\n")
    r = _run(tmp_path, 'strih_intercom_hub_install_fingerprint "${FP[0]}" "${FP[1]}" "${FP[2]}" "$1"',
             etc / "intercom.toml")
    paths = [l.split("  ", 1)[1] for l in r.stdout.splitlines()]
    assert str(etc / "turn.toml") in paths and paths.count(str(etc / "intercom.toml")) == 1, r.stdout


def test_a_stopped_hub_is_never_started_or_restarted_here(tmp_path):
    r = _changed_run(tmp_path, lambda b: (b / "hub").write_text("hub 1.9.13\n"), env={"FAKE_ACTIVE": "0"})
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    c = _calls(tmp_path)
    assert "try-restart" not in c and "start" not in c.replace("is-active", ""), c


def test_no_before_fingerprint_restarts_nothing(tmp_path):
    _box(tmp_path)
    r = _run(tmp_path, 'strih_intercom_hub_restart_if_changed "" "${FP[@]}"\necho "RC=$?"')
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert "try-restart" not in _calls(tmp_path)


def test_a_failed_restart_or_a_crash_loop_after_it_fails_by_name(tmp_path):
    t1 = tmp_path / "a"
    t1.mkdir()
    r = _changed_run(t1, lambda b: (b / "hub").write_text("new\n"), env={"FAKE_RESTART_RC": "1"})
    assert r.returncode != 0 and "RC=" not in r.stdout, r.stdout   # rc 1 ends the caller's set -e
    assert "try-restart" in r.stderr + r.stdout, r.stdout + r.stderr
    t2 = tmp_path / "b"
    t2.mkdir()
    r = _changed_run(t2, lambda b: (b / "hub").write_text("new\n"), env={"FAKE_SUB": "auto-restart"})
    assert r.returncode != 0 and "RC=" not in r.stdout, r.stdout + r.stderr   # rc 1 ends the caller's set -e
    assert "crash-looping" in r.stdout + r.stderr, r.stdout + r.stderr


# --- the setup-strih wiring ----------------------------------------------------------------------------------

def test_setup_takes_the_fingerprint_before_any_hub_write_and_restarts_after_the_last_one():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    fp = s.index('IH_BEFORE="$(strih_intercom_hub_install_fingerprint)"')
    assert fp < s.index('step 12 "'), "before step 12 / 13 write any hub file"
    assert fp < s.index("strih_intercom_hub_binary_install /usr/local/bin/intercom-hub")
    live = s[s.index('if WHY="$(obs_box_step_applies 13-hub-live)"'):s.index('step 15 "')]
    assert s.index('step 14 "') < s.index('if WHY="$(obs_box_step_applies 13-hub-live)"')
    start = live.index("systemctl start intercom-hub")
    assert "strih_intercom_hub_record_loaded" in live[start:start + 400], "a started hub records its install"
    assert s.count('strih_intercom_hub_restart_if_changed "${IH_BEFORE:-}"') == 1
    restart = s.index('strih_intercom_hub_restart_if_changed "${IH_BEFORE:-}"')
    assert s.index('step "16i"') < restart < s.index('step "16j"'), "right after 16i's TURN drop-in, before verify"
    line = s[s.rindex("\n", 0, restart):s.index("\n", restart)]
    assert "obs_box_step_applies 13-hub-live" in line and "|| fail" in line, line


# --- the deploy read-back ------------------------------------------------------------------------------------

def _verdict(*args):
    script = '. "%s"\nstrih_lx_deploy_verdict "$@"\necho "RC=$?"' % DEPLOY
    return subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True,
                          cwd=str(REPO), timeout=30)


BASE = ["abc123", "abc123", "active", "abc123", "lib1", "lib1", "1", "v9.9.5", "v9.9.5"]


def test_the_read_back_fails_a_running_hub_off_the_pin():
    r = _verdict(*BASE, "9.9.3", "9.9.2")
    assert "RC=1" in r.stdout, r.stdout + r.stderr
    assert "intercom hub" in r.stdout and "9.9.2" in r.stdout and "9.9.3" in r.stdout, r.stdout
    r = _verdict(*BASE, "9.9.3", "")
    assert "RC=1" in r.stdout and "<unreadable>" in r.stdout, r.stdout


def test_the_read_back_passes_the_pinned_hub_and_a_box_without_a_hub():
    assert _verdict(*BASE, "9.9.3", "9.9.3").stdout.split() == ["OK", "RC=0"]
    assert _verdict(*BASE, "", "").stdout.split() == ["OK", "RC=0"]
    assert _verdict(*BASE).stdout.split() == ["OK", "RC=0"]


def test_the_remote_read_back_reads_the_running_hubs_version(tmp_path):
    r = subprocess.run(["bash", "-c", '. "%s"\nstrih_lx_remote_readback_cmd "$1"' % DEPLOY, "harness",
                        str(tmp_path)], capture_output=True, text=True, cwd=str(REPO), timeout=30)
    assert r.returncode == 0, r.stderr
    assert "127.0.0.1:8790/api/version" in r.stdout, r.stdout
    # the printed remote command really runs: a fake curl answers the hub's /api/version shape
    fake = tmp_path / "bin"
    fake.mkdir()
    (fake / "curl").write_text('#!/bin/sh\nprintf \'{"version": "1.9.13-rc1", "build_version": "7.0"}\'\n')
    (fake / "systemctl").write_text("#!/bin/sh\necho active\n")
    for f in ("curl", "systemctl"):
        (fake / f).chmod(0o755)
    home = tmp_path / "home"
    home.mkdir()
    out = subprocess.run(["bash", "-c", r.stdout], capture_output=True, text=True, timeout=30,
                         env=dict(os.environ, HOME=str(home), PATH="%s:%s" % (fake, os.environ["PATH"])))
    fields = dict(f.split("=", 1) for f in out.stdout.split() if "=" in f)
    assert fields.get("hub") == "1.9.13-rc1", out.stdout + out.stderr   # the JSON key, never a regex


def test_the_deploy_wants_the_pinned_hub_on_the_strih_role_only():
    s = DEPLOY.read_text()
    assert 'STRIH_LX_BOX_HUB[$b]' in s
    assert '"$(_strih_lx_field hub "$line")"' in s
