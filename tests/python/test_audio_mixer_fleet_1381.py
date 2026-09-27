"""issue 1381 -- the audio-mixer pager's fleet wiring: the obs-fleet `audio-mixer` facet, the
watchdog's roster derivation + traveling gate, the dev1 timer roster, and the DISABLED-by-default
systemd units. Tier-0 (bash + pytest, no cargo)."""
import os
import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_WATCHDOG = _SCRIPTS / "audio-mixer-alert-watchdog.sh"


def _bash(script, env=None):
    full = dict(os.environ)
    full.update(env or {})
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=full,
                          timeout=30)


def test_obs_fleet_audio_mixer_facet_is_every_obs_box_that_mixes_audio():
    r = _bash(f'. "{_SCRIPTS}/lib/obs-fleet.sh"; obs_fleet_facet_members audio-mixer')
    assert r.returncode == 0, r.stderr
    assert r.stdout == "strih-lx stream resolume"
    r = _bash(f'. "{_SCRIPTS}/lib/obs-fleet.sh"; obs_fleet_boxes audio-mixer')
    assert r.stdout == "strih-lx|10.77.9.202 stream|10.77.9.204 resolume|resolume.lan"


def test_obs_fleet_unknown_facet_message_names_audio_mixer():
    r = _bash(f'. "{_SCRIPTS}/lib/obs-fleet.sh"; obs_fleet_facet_members nope')
    assert r.returncode != 0
    assert "audio-mixer" in r.stderr


def test_watchdog_derives_its_roster_from_the_fleet_facet():
    src = _WATCHDOG.read_text(encoding="utf-8")
    assert "lib/obs-fleet.sh" in src
    assert 'BOXES="${AUDIO_MIXER_BOXES:-$(obs_fleet_boxes audio-mixer)}"' in src
    assert "obs_fleet_poll_now" in src


def test_watchdog_skips_an_away_resolume_without_fetching(tmp_path):
    # resolume is traveling: when it is not home it is not polled at all (no fetch, no verdict).
    fetch = tmp_path / "fetch.sh"
    marker = tmp_path / "fetched"
    fetch.write_text(f"#!/usr/bin/env bash\ntouch '{marker}'\necho '{{}}'\n", encoding="utf-8")
    fetch.chmod(0o755)
    # The OBS_FLEET_HOME force-list seam names only strih-lx, so resolume reads AWAY offline.
    env = {"AUDIO_MIXER_FETCH_CMD": str(fetch), "AUDIO_MIXER_BOXES": "resolume|resolume.lan",
           "AUDIO_MIXER_ALERT_STATE_FILE": str(tmp_path / "st"), "OBS_FLEET_HOME": "strih-lx"}
    r = _bash(f'bash "{_WATCHDOG}" --dry-run', env)
    assert r.returncode == 0, r.stderr
    assert "resolume" in r.stderr and "away" in r.stderr
    assert not marker.exists()


def test_watchdog_polls_a_home_resolume(tmp_path):
    fetch = tmp_path / "fetch.sh"
    marker = tmp_path / "fetched"
    fetch.write_text(f"#!/usr/bin/env bash\ntouch '{marker}'\necho '{{}}'\n", encoding="utf-8")
    fetch.chmod(0o755)
    env = {"AUDIO_MIXER_FETCH_CMD": str(fetch), "AUDIO_MIXER_BOXES": "resolume|resolume.lan",
           "AUDIO_MIXER_ALERT_STATE_FILE": str(tmp_path / "st"), "OBS_FLEET_HOME": "resolume"}
    r = _bash(f'bash "{_WATCHDOG}" --dry-run', env)
    assert r.returncode == 0, r.stderr
    assert marker.exists()
    assert "mixer=UNKNOWN" in r.stderr and "vban=UNKNOWN" in r.stderr


def test_watchdog_passes_bash_syntax_check():
    r = subprocess.run(["bash", "-n", str(_WATCHDOG)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def test_timer_is_on_the_dev1_watchdog_roster():
    roster = (_SCRIPTS / "lib" / "watchdog-roster.sh").read_text(encoding="utf-8")
    assert "audio-mixer-alert-watchdog.timer:core" in roster


def test_systemd_units_ship_and_point_at_the_watchdog():
    svc = (_ROOT / "systemd" / "audio-mixer-alert-watchdog.service").read_text(encoding="utf-8")
    tmr = (_ROOT / "systemd" / "audio-mixer-alert-watchdog.timer").read_text(encoding="utf-8")
    assert "ExecStart=%h/devel/camera-box/scripts/audio-mixer-alert-watchdog.sh" in svc
    assert "Type=oneshot" in svc
    assert "OnUnitActiveSec=5min" in tmr
    assert (_ROOT / "systemd" / "audio-mixer-alert-watchdog.README.md").exists()
