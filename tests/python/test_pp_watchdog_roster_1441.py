"""issue 1441 slice 2 -- the dev1 alert watchdogs page for the Poprad OBS boxes (strih-pp,
stream-pp) like for the SNV ones, by box CLASS and home-check, never by box name.

Owner ruling 15.10.2026: one unified PP/SNV solution. Both PP boxes are `linux-genlock` rows with
home-check `traveling` (PP is often powered off: the normal state). So for every roster watchdog:

  * PP away  -> the box is not polled (or is report-only) and never pages;
  * PP home + healthy -> no page;
  * PP home + fault   -> a page naming the box, after the watchdog's own confirm;
  * a linux-genlock box never gets a Windows action (schtasks / the launch-obs-genlock plan).

Every run is a real `--dry-run` pass of the watchdog script. The bundle-state body is served by a
`*_FETCH_CMD` replay script (no curl), `OBS_FLEET_HOME` pins the home check (no live probe), the
ping / curl probes of network-reach and bundle-state are PATH stubs and their TCP ports point at
closed loopback ports, and every state file lives in the test's tmp dir. Tier-0: bash + python3,
no network.
"""
from __future__ import annotations

import json
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_FLEET_LIB = _SCRIPTS / "lib" / "obs-fleet.sh"

PP = ("strih-pp", "stream-pp")
PP_PAIRS = ("strih-pp|strih-pp.lan", "stream-pp|stream-pp.lan")

# The facets the design (comment 6193925308) adds the PP boxes to. audio-lag is included because
# both PP boxes serve the `audio_ts_lag_*` facet on :8899 (read-only check 15.10.2026).
PP_FACETS = ("network-reach", "bundle-state", "obs-liveness", "genlock-lock", "render-freeze",
             "audio-mixer", "obs-handles", "audio-lag")
# Windows-only, stream-snv-only or SNV measurement-rig premises: the PP boxes stay out.
NOT_PP_FACETS = ("win-baseline", "vb-matrix", "av-step", "obs-session", "burn-reconcile",
                 "rig-restore", "ndi-portmap")

# A healthy :8899 body as strih-pp served it on 15.10.2026 (the keys the watchdogs read).
_HEALTHY = {
    "obs_version": "32.2.0",
    "genlock_build_sha": "b" * 40,
    "audio_ts_lag_ms": "106",
    "audio_ts_lag_src": "ASIO zvuk",
    "audio_ts_lag_age_s": "44",
    "program_render_lagged": "0",
    "program_render_lagged_age_s": "2",
    "audio_mixer_ticks": "2813",
    "audio_mixer_ticks_over": "0",
    "audio_mixer_window_ms": "60011",
    "audio_mixer_tick_ms": "21.3",
    "audio_mixer_age_s": "44",
    "obs_log_head_age_s": "0",
    "obs_handles": "484",
    "obs_handles_pid": "3199186",
    "obs_handles_start": "1791973883",
    "obs_handles_run": "ce29f8a3-19de-4ba3-919b-749b3447dbf7:24218955",
    "obs_handles_limit": "524288",
    "genlock_lock": {
        "state": "LOCKED", "reason": "none", "n_inputs": 8, "n_locked": 5, "n_absent": 3,
        "n_idle": 0, "latency_ms": 3, "recent_event": False,
        "clock": {"state": "locked"},
        "output": {"present": True, "stamping_wallclock": True},
    },
}


def _body(**over):
    b = json.loads(json.dumps(_HEALTHY))
    for k, v in over.items():
        b[k] = v
    return b


def _bash(script, *args, env=None):
    r = subprocess.run(["bash", "-c", script, "harness", *args], capture_output=True, text=True,
                       env=env, timeout=60, cwd=_ROOT)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _fleet(fn, *args):
    return _bash(f'set -euo pipefail\n. "{_FLEET_LIB}"\n{fn} "$@"', *args)


# ------------------------------------------------------------------------------------------------
# The one policy: obs_fleet_boxes <facet>
# ------------------------------------------------------------------------------------------------
def test_every_pp_facet_carries_both_pp_boxes():
    for facet in PP_FACETS:
        roster = _fleet("obs_fleet_boxes", facet).split()
        for pair in PP_PAIRS:
            assert pair in roster, f"facet {facet} must carry {pair}: {roster}"


def test_pp_boxes_stay_out_of_the_windows_and_snv_rig_facets():
    for facet in NOT_PP_FACETS:
        roster = _fleet("obs_fleet_boxes", facet)
        for box in PP:
            assert box not in roster, f"facet {facet} must NOT carry {box}: {roster}"


def test_the_snv_members_of_each_pp_facet_are_unchanged():
    snv = {
        "network-reach": "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan",
        "bundle-state": "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan",
        "obs-liveness": "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan",
        "genlock-lock": "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan",
        "render-freeze": "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan",
        "audio-mixer": "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan",
        "obs-handles": "strih-snv|10.77.9.202 stream-snv|10.77.9.204 resolume-snv|resolume.lan",
        "audio-lag": "strih-snv|10.77.9.202 stream-snv|10.77.9.204",
    }
    for facet, prefix in snv.items():
        roster = _fleet("obs_fleet_boxes", facet)
        assert roster == prefix + " " + " ".join(PP_PAIRS), f"{facet}: {roster}"


def test_resolume_pp_joins_no_new_facet():
    # resolume-pp (Windows, no :8899 yet -- issue 1426) is not part of this slice.
    for facet in PP_FACETS:
        assert "resolume-pp" not in _fleet("obs_fleet_boxes", facet), facet


# ------------------------------------------------------------------------------------------------
# A replay harness shared by the :8899-reading watchdogs.
# ------------------------------------------------------------------------------------------------
class _Replay:
    """Passes of one watchdog over a scratch state file, the body served per HOST by a fetch
    script (a host with no body file reads as unreachable -> SKIP)."""

    def __init__(self, tmp_path, script, seam, state_env, home, extra_env=None):
        self.tmp = tmp_path
        self.script = _SCRIPTS / script
        self.fetched = tmp_path / "fetched"
        fetch = tmp_path / "fetch.sh"
        fetch.write_text(
            "#!/usr/bin/env bash\n"
            f"echo \"$1\" >> '{self.fetched}'\n"
            f"f='{tmp_path}/'\"$1\"'.json'\n"
            "[ -f \"$f\" ] || exit 1\n"
            "cat \"$f\"\n", encoding="utf-8")
        fetch.chmod(0o755)
        self.env = dict(os.environ)
        self.env.pop("REPING_INTERVAL_S", None)
        self.env.update({seam: str(fetch), state_env: str(tmp_path / "state"),
                         "OBS_FLEET_HOME": home})
        self.env.update(extra_env or {})

    def serve(self, host, body):
        p = self.tmp / f"{host}.json"
        if body is None:
            p.unlink(missing_ok=True)
        else:
            p.write_text(json.dumps(body), encoding="utf-8")

    def run(self, extra_env=None):
        env = dict(self.env, **(extra_env or {}))
        r = subprocess.run(["bash", str(self.script), "--dry-run"], capture_output=True, text=True,
                           env=env, timeout=120, cwd=_ROOT)
        assert r.returncode == 0, r.stderr
        return r.stdout + r.stderr

    def fetched_hosts(self):
        if not self.fetched.exists():
            return []
        return self.fetched.read_text(encoding="utf-8").split()


def _would_alerts(out, box):
    return [ln for ln in out.splitlines() if "WOULD alert" in ln and box in ln]


# Each entry: script, fetch seam, state env, the fault body, extra env, passes until the page.
_FACET_WATCHDOGS = {
    "genlock-lock": ("genlock-lock-alert-watchdog.sh", "GENLOCK_LOCK_FETCH_CMD",
                     "GENLOCK_LOCK_ALERT_STATE_FILE",
                     lambda: _body(genlock_lock={"state": "UNLOCKED", "reason": "clock",
                                                 "n_inputs": 8, "n_locked": 0, "n_absent": 0,
                                                 "n_idle": 0, "latency_ms": 3,
                                                 "recent_event": False,
                                                 "clock": {"state": "unlocked"},
                                                 "output": {"present": True,
                                                            "stamping_wallclock": True}}),
                     {}, 2),
    "render-freeze": ("render-freeze-alert-watchdog.sh", "RENDER_FREEZE_FETCH_CMD",
                      "RENDER_FREEZE_ALERT_STATE_FILE",
                      lambda: _body(program_render_lagged="228", program_render_lagged_age_s="10"),
                      {}, 2),
    "audio-lag": ("audio-lag-alert-watchdog.sh", "AUDIO_LAG_FETCH_CMD",
                  "AUDIO_LAG_ALERT_STATE_FILE",
                  lambda: _body(audio_ts_lag_ms="20000", audio_ts_lag_age_s="5"),
                  {}, 2),
    "audio-mixer": ("audio-mixer-alert-watchdog.sh", "AUDIO_MIXER_FETCH_CMD",
                    "AUDIO_MIXER_ALERT_STATE_FILE",
                    lambda: _body(audio_mixer_ticks="2500"),
                    {}, 2),
    "obs-handles": ("obs-handles-alert-watchdog.sh", "OBS_HANDLES_FETCH_CMD",
                    "OBS_HANDLES_ALERT_STATE_FILE",
                    lambda: _body(obs_handles="450000"),
                    {}, 3),
}


def _replay(tmp_path, facet, home):
    script, seam, state_env, _fault, extra, _n = _FACET_WATCHDOGS[facet]
    return _Replay(tmp_path, script, seam, state_env, home, extra)


def _passes(rep, n, step_env=None):
    outs = []
    for i in range(n):
        env = {}
        if step_env:
            env = step_env(i)
        outs.append(rep.run(env))
    return outs


def _now_env(facet):
    # obs-handles reads the pass time from OBS_HANDLES_NOW_EPOCH (its reference interval);
    # audio-mixer from AUDIO_MIXER_NOW_EPOCH. 300 s apart = the 5-min timer cadence.
    var = {"obs-handles": "OBS_HANDLES_NOW_EPOCH", "audio-mixer": "AUDIO_MIXER_NOW_EPOCH"}.get(facet)
    if not var:
        return None
    return lambda i: {var: str(1_792_000_000 + 300 * i)}


def _check_pp_away(tmp_path, facet):
    rep = _replay(tmp_path, facet, home="stream-snv")
    for host in ("strih-pp.lan", "stream-pp.lan"):
        rep.serve(host, _FACET_WATCHDOGS[facet][3]())   # a fault body: still never read
    outs = _passes(rep, 3, _now_env(facet))
    for out in outs:
        for box in PP:
            assert not _would_alerts(out, box), out
    fetched = rep.fetched_hosts()
    assert "strih-pp.lan" not in fetched and "stream-pp.lan" not in fetched, fetched
    assert any("strih-pp" in ln and "away" in ln for ln in outs[0].splitlines()), outs[0]


def _check_pp_home_healthy(tmp_path, facet):
    rep = _replay(tmp_path, facet, home="strih-pp stream-pp")
    rep.serve("strih-pp.lan", _body())
    rep.serve("stream-pp.lan", _body())
    outs = _passes(rep, 3, _now_env(facet))
    for out in outs:
        for box in PP:
            assert not _would_alerts(out, box), out
    assert "strih-pp.lan" in rep.fetched_hosts()


def _check_pp_home_fault_pages(tmp_path, facet):
    _s, _seam, _st, fault, _x, n = _FACET_WATCHDOGS[facet]
    rep = _replay(tmp_path, facet, home="strih-pp stream-pp")
    rep.serve("strih-pp.lan", fault())
    rep.serve("stream-pp.lan", _body())
    outs = _passes(rep, n + 1, _now_env(facet))
    paged = [ln for out in outs for ln in _would_alerts(out, "strih-pp")]
    assert paged, "\n".join(outs)
    for out in outs:
        assert not _would_alerts(out, "stream-pp"), out
    # One pass alone never pages (each watchdog's confirm).
    assert not _would_alerts(outs[0], "strih-pp"), outs[0]


def test_genlock_lock_pp_away_never_polled(tmp_path):
    _check_pp_away(tmp_path, "genlock-lock")


def test_genlock_lock_pp_home_healthy_no_page(tmp_path):
    _check_pp_home_healthy(tmp_path, "genlock-lock")


def test_genlock_lock_pp_home_unlocked_pages(tmp_path):
    _check_pp_home_fault_pages(tmp_path, "genlock-lock")


def test_render_freeze_pp_away_never_polled(tmp_path):
    _check_pp_away(tmp_path, "render-freeze")


def test_render_freeze_pp_home_healthy_no_page(tmp_path):
    _check_pp_home_healthy(tmp_path, "render-freeze")


def test_render_freeze_pp_home_freeze_pages(tmp_path):
    _check_pp_home_fault_pages(tmp_path, "render-freeze")


def test_audio_lag_pp_away_never_polled(tmp_path):
    _check_pp_away(tmp_path, "audio-lag")


def test_audio_lag_pp_home_healthy_no_page(tmp_path):
    _check_pp_home_healthy(tmp_path, "audio-lag")


def test_audio_lag_pp_home_lagging_pages(tmp_path):
    _check_pp_home_fault_pages(tmp_path, "audio-lag")


def test_audio_mixer_pp_away_never_polled(tmp_path):
    _check_pp_away(tmp_path, "audio-mixer")


def test_audio_mixer_pp_home_healthy_no_page(tmp_path):
    _check_pp_home_healthy(tmp_path, "audio-mixer")


def test_audio_mixer_pp_home_behind_pages(tmp_path):
    _check_pp_home_fault_pages(tmp_path, "audio-mixer")


def test_obs_handles_pp_away_never_polled(tmp_path):
    _check_pp_away(tmp_path, "obs-handles")


def test_obs_handles_pp_home_healthy_no_page(tmp_path):
    _check_pp_home_healthy(tmp_path, "obs-handles")


def test_obs_handles_pp_home_ceiling_pages(tmp_path):
    _check_pp_home_fault_pages(tmp_path, "obs-handles")


def test_a_snv_always_box_is_still_polled_with_a_pp_home_list(tmp_path):
    # The home list names only PP boxes; strih-snv (`always`) must still be read every pass.
    for facet in ("genlock-lock", "render-freeze", "audio-lag", "audio-mixer", "obs-handles"):
        sub = tmp_path / facet
        sub.mkdir()
        rep = _replay(sub, facet, home="strih-pp")
        rep.serve("10.77.9.202", _body())
        rep.run((_now_env(facet) or (lambda i: {}))(0))
        assert "10.77.9.202" in rep.fetched_hosts(), facet


# ------------------------------------------------------------------------------------------------
# network-reach and bundle-state: probe-based (ping / TCP / curl), stubbed on PATH.
# ------------------------------------------------------------------------------------------------
def _stub_bin(tmp_path):
    b = tmp_path / "bin"
    b.mkdir(exist_ok=True)
    ping = b / "ping"
    ping.write_text('#!/usr/bin/env bash\nip="${*: -1}"\n'
                    'case " ${TEST_PING_UP:-} " in *" $ip "*) exit 0;; *) exit 1;; esac\n',
                    encoding="utf-8")
    curl = b / "curl"
    curl.write_text('#!/usr/bin/env bash\n'
                    '[ "${TEST_CURL:-down}" = up ] || exit 7\nprintf \'{"ok":"1"}\'\n',
                    encoding="utf-8")
    for p in (ping, curl):
        p.chmod(0o755)
    return b


def _probe_run(tmp_path, script, env):
    full = dict(os.environ)
    full["PATH"] = f"{_stub_bin(tmp_path)}:{full.get('PATH', '')}"
    full.update(env)
    r = subprocess.run(["bash", str(_SCRIPTS / script), "--dry-run"], capture_output=True,
                       text=True, env=full, timeout=120, cwd=_ROOT)
    assert r.returncode == 0, r.stderr
    return r.stdout + r.stderr


def _netreach_env(tmp_path, home, ping_up):
    return {
        "NETWORK_REACH_BOXES": "strih-snv|127.0.0.3 strih-pp|127.0.0.1 stream-pp|127.0.0.2",
        "NETWORK_REACH_REFERENCE_HOSTS": "127.0.0.9",
        "NETWORK_REACH_OBS_WS_PORT": "65401",
        "NETWORK_REACH_BUNDLE_PORT": "65402",
        "NETWORK_REACH_TCP_TIMEOUT": "2",
        "NETWORK_REACH_PING_TIMEOUT": "1",
        "NETWORK_REACH_ALERT_STATE_FILE": str(tmp_path / "nr.state"),
        "OBS_FLEET_HOME": home,
        "TEST_PING_UP": ping_up,
    }


# Source the watchdog with no positional arguments (it parses $1 as its CLI flag) and print one var.
_SOURCE_AND_PRINT_RO = 'set -uo pipefail\nwd="$1"\nset --\n. "$wd"\nprintf "%s" "$REPORT_ONLY_BOXES"'


def test_network_reach_report_only_default_is_every_away_traveling_box():
    def ro(home):
        env = dict(os.environ, OBS_FLEET_HOME=home)
        env.pop("NETWORK_REACH_REPORT_ONLY_BOXES", None)
        env.pop("NETWORK_REACH_BOXES", None)
        return _bash(_SOURCE_AND_PRINT_RO, str(_SCRIPTS / "network-reach-alert-watchdog.sh"),
                     env=env)

    assert ro("stream-snv").split() == ["resolume-snv", "strih-pp", "stream-pp"]
    assert ro("strih-pp").split() == ["resolume-snv", "stream-pp"]
    assert ro("resolume-snv strih-pp stream-pp") == ""


def test_network_reach_report_only_is_keyed_on_the_home_check_not_a_name():
    # A traveling row the code has never heard of is report-only while away, by its home-check.
    env = dict(os.environ, OBS_FLEET_HOME="stream-snv",
               OBS_FLEET="strih-snv|10.77.9.202|linux-genlock|always\n"
                         "stream-snv|10.77.9.204|windows-genlock|always\n"
                         "strih-zz|strih-zz.lan|linux-genlock|traveling",
               NETWORK_REACH_BOXES="strih-snv|10.77.9.202 strih-zz|strih-zz.lan")
    env.pop("NETWORK_REACH_REPORT_ONLY_BOXES", None)
    out = _bash(_SOURCE_AND_PRINT_RO, str(_SCRIPTS / "network-reach-alert-watchdog.sh"), env=env)
    assert out == "strih-zz"


def test_network_reach_pp_away_never_pages(tmp_path):
    env = _netreach_env(tmp_path, "stream-snv", "127.0.0.9 127.0.0.3")
    outs = [_probe_run(tmp_path, "network-reach-alert-watchdog.sh", env) for _ in range(3)]
    for out in outs:
        for box in PP:
            assert not _would_alerts(out, box), out
    assert "[report-only] strih-pp CONFIRMED unreachable" in outs[-1]


def test_network_reach_pp_home_unreachable_pages(tmp_path):
    env = _netreach_env(tmp_path, "strih-pp stream-pp", "127.0.0.9 127.0.0.3 127.0.0.2")
    outs = [_probe_run(tmp_path, "network-reach-alert-watchdog.sh", env) for _ in range(2)]
    assert not _would_alerts(outs[0], "strih-pp"), outs[0]
    assert _would_alerts(outs[1], "strih-pp"), outs[1]
    assert not _would_alerts(outs[1], "stream-pp"), outs[1]


def test_network_reach_pp_home_reachable_no_page(tmp_path):
    env = _netreach_env(tmp_path, "strih-pp stream-pp", "127.0.0.9 127.0.0.3 127.0.0.1 127.0.0.2")
    outs = [_probe_run(tmp_path, "network-reach-alert-watchdog.sh", env) for _ in range(3)]
    for out in outs:
        for box in PP:
            assert not _would_alerts(out, box), out
    assert "strih-pp (127.0.0.1)" in outs[0] and "-> REACHABLE" in outs[0]


def _bundle_env(tmp_path, home, ping_up, curl):
    return {
        "BUNDLE_STATE_BOXES": "strih-pp|127.0.0.1",
        "BUNDLE_STATE_REFERENCE_HOSTS": "127.0.0.9",
        "BUNDLE_STATE_OBS_WS_PORT": "45999",
        "BUNDLE_STATE_TCP_TIMEOUT": "2",
        "BUNDLE_STATE_ALERT_STATE_FILE": str(tmp_path / "bs.state"),
        "OBS_FLEET_HOME": home,
        "TEST_PING_UP": ping_up,
        "TEST_CURL": curl,
    }


def test_bundle_state_pp_away_is_not_probed(tmp_path):
    env = _bundle_env(tmp_path, "stream-snv", "127.0.0.9 127.0.0.1", "down")
    outs = [_probe_run(tmp_path, "bundle-state-alert-watchdog.sh", env) for _ in range(3)]
    for out in outs:
        assert not _would_alerts(out, "strih-pp"), out
        assert "WOULD auto-restart" not in out, out
    assert any("strih-pp" in ln and "away" in ln for ln in outs[0].splitlines()), outs[0]


def test_bundle_state_pp_home_healthy_no_action(tmp_path):
    env = _bundle_env(tmp_path, "strih-pp", "127.0.0.9 127.0.0.1", "up")
    outs = [_probe_run(tmp_path, "bundle-state-alert-watchdog.sh", env) for _ in range(3)]
    for out in outs:
        assert not _would_alerts(out, "strih-pp"), out
        assert "-> HEALTHY" in out, out


def test_bundle_state_pp_home_down_restarts_its_systemd_unit_never_schtasks(tmp_path):
    env = _bundle_env(tmp_path, "strih-pp", "127.0.0.9 127.0.0.1", "down")
    outs = [_probe_run(tmp_path, "bundle-state-alert-watchdog.sh", env) for _ in range(2)]
    assert not _would_alerts(outs[0], "strih-pp"), outs[0]
    assert _would_alerts(outs[1], "strih-pp"), outs[1]
    assert "systemctl --user restart '*-bundle-state-server.service'" in outs[1], outs[1]
    assert "schtasks" not in "".join(outs)


# ------------------------------------------------------------------------------------------------
# obs-liveness: the poll set + the class-resolved recovery plan.
# ------------------------------------------------------------------------------------------------
_LIVENESS = _SCRIPTS / "obs-liveness-watchdog.sh"


def _probe_stub(tmp_path):
    """A stand-in for obs-liveness-probe.py: one verdict line per --box; a box named in
    TEST_WEDGED reads FPS-ZERO, the rest HEALTHY. Every --box value is logged."""
    p = tmp_path / "probe.py"
    p.write_text(
        "import os, sys\n"
        "args = sys.argv[1:]\n"
        "wedged = os.environ.get('TEST_WEDGED', '').split()\n"
        "log = os.environ.get('TEST_PROBE_LOG')\n"
        "for i, a in enumerate(args):\n"
        "    if a == '--box':\n"
        "        spec = args[i + 1]\n"
        "        name = spec.split('=', 1)[0]\n"
        "        if log:\n"
        "            open(log, 'a').write(spec + '\\n')\n"
        "        if name in wedged:\n"
        "            print('FPS-ZERO %s: render stalled' % name)\n"
        "        else:\n"
        "            print('HEALTHY %s: ok' % name)\n", encoding="utf-8")
    return p


def _liveness_run(tmp_path, home, wedged=""):
    log = tmp_path / "probe.log"
    env = dict(os.environ, OBS_FLEET_HOME=home, OBS_LIVENESS_PROBE=str(_probe_stub(tmp_path)),
               OBS_WATCHDOG_STATE_FILE=str(tmp_path / "ol.state"), TEST_WEDGED=wedged,
               TEST_PROBE_LOG=str(log))
    r = subprocess.run(["bash", str(_LIVENESS), "--dry-run"], capture_output=True, text=True,
                       env=env, timeout=120, cwd=_ROOT)
    assert r.returncode == 0, r.stderr
    specs = log.read_text(encoding="utf-8").split() if log.exists() else []
    log.unlink(missing_ok=True)
    return r.stdout + r.stderr, specs


def test_obs_liveness_pp_away_not_polled(tmp_path):
    out, specs = _liveness_run(tmp_path, "stream-snv", wedged="strih-pp stream-pp")
    assert not any(s.startswith(("strih-pp=", "stream-pp=")) for s in specs), specs
    assert any(s.startswith("strih-snv=") for s in specs), specs
    for box in PP:
        assert not _would_alerts(out, box), out


def test_obs_liveness_pp_home_is_polled_at_30fps(tmp_path):
    _out, specs = _liveness_run(tmp_path, "strih-pp stream-pp")
    assert "strih-pp=strih-pp.lan:30" in specs, specs
    assert "stream-pp=stream-pp.lan:30" in specs, specs
    assert any(s.startswith("strih-snv=") for s in specs), specs


def test_obs_liveness_pp_home_wedged_pages_with_the_linux_plan(tmp_path):
    out1, _ = _liveness_run(tmp_path, "strih-pp stream-pp", wedged="strih-pp")
    out2, _ = _liveness_run(tmp_path, "strih-pp stream-pp", wedged="strih-pp")
    assert not _would_alerts(out1, "strih-pp"), out1
    alerts = _would_alerts(out2, "strih-pp")
    assert alerts, out2
    assert "systemctl --user" in alerts[0] and "strih-pp.lan" in alerts[0], alerts[0]
    assert "launch-obs-genlock" not in alerts[0], alerts[0]
    assert not _would_alerts(out2, "stream-pp"), out2


def test_obs_liveness_resolume_snv_is_still_polled_only_while_home(tmp_path):
    _out, away = _liveness_run(tmp_path, "stream-snv")
    assert not any(s.startswith("resolume-snv=") for s in away), away
    _out, home = _liveness_run(tmp_path, "resolume-snv")
    assert "resolume-snv=resolume.lan:60" in home, home


# ------------------------------------------------------------------------------------------------
# The development handover check reads network-reach's dry-run lines: an away traveling box is a
# report-only line (PP powered off is the normal state), so it must never read as "zabudol si".
# ------------------------------------------------------------------------------------------------
def _handover_decision():
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        "rig_dev_handover_decision_1441", _SCRIPTS / "rig_dev_handover_decision.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_handover_net_item_ignores_a_report_only_away_box():
    d = _handover_decision()
    text = ("x [network-reach-alert-watchdog] strih-snv (10.77.9.202): ping=1 ws:4455=1 "
            "bundle:8899=1 report_only=0 -> REACHABLE\n"
            "x [network-reach-alert-watchdog] strih-pp (strih-pp.lan): ping=0 ws:4455=0 "
            "bundle:8899=0 report_only=1 -> UNREACHABLE\n"
            "x [network-reach-alert-watchdog] stream-pp (stream-pp.lan): ping=0 ws:4455=0 "
            "bundle:8899=0 report_only=1 -> UNREACHABLE\n")
    assert d.net_tokens(text) == ["REACHABLE"]
    # A paging (report_only=0) box that is down is still counted.
    down = text.replace("strih-pp (strih-pp.lan): ping=0 ws:4455=0 bundle:8899=0 report_only=1",
                        "strih-pp (strih-pp.lan): ping=0 ws:4455=0 bundle:8899=0 report_only=0")
    assert d.net_tokens(down) == ["REACHABLE", "UNREACHABLE"]
