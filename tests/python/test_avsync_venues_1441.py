"""issue 1441 -- the ONE per-venue table of the lipsync (SyncNet) measurement: which stream box each
venue's A/V is measured on, the RTMP URL dev2 pulls and the OBS-WS host dev1 reads outputActive from.

`scripts/lib/avsync-venues.sh` derives it from the declared fleet, never from a hand-typed address:
- a Linux stream box = a fact file `scripts/strih-boxes/<box>.env` with STRIH_ROLE=stream (venue =
  STRIH_VENUE, stream key = the box name, the issue-1436 convention);
- the hand-built Windows SNV stream box = the one declared row `snv|stream-snv|obs-e2e-test` (it has
  no fact file until it moves to the Linux stream role, owner ROZHODNUTÉ 15.10.2026);
- the dial address = the box's obs-fleet row host (the address dev1/dev2 reach the box by; the PP
  boxes' own STRIH_IP 10.77.8.x is not reachable from dev1/dev2, measured on issue 1441).

The lib is sourced under the caller's `set -euo pipefail` (the measurer and both dev1 watchdogs'
source time), so every test sources it that way.
"""

import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "avsync-venues.sh"


def _run(body, env_extra=None, check=True):
    env = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent-home-1441", "LIB": str(_LIB)}
    if env_extra:
        env.update(env_extra)
    harness = 'set -euo pipefail\n. "$LIB"\n' + body
    proc = subprocess.run(["bash", "-c", harness], env=env, capture_output=True, text=True)
    if check:
        assert proc.returncode == 0, (
            f"harness exited {proc.returncode}\nstdout={proc.stdout!r}\nstderr={proc.stderr!r}"
        )
    return proc


def _write_box(d, name, role, venue):
    """A minimal fact file: the venue table parses it with the loader's parser, which only needs
    KEY=value lines of known keys (validation of the full fact set is the loader's own business)."""
    (d / f"{name}.env").write_text(
        f"STRIH_HOSTNAME={name}\nSTRIH_IP=10.99.0.4\nSTRIH_ROLE={role}\nSTRIH_VENUE={venue}\n"
    )


# ── the real repo: SNV from the declared Windows row, PP from stream-pp.env ──────────────────────


def test_default_table_has_snv_then_pp_from_the_declared_fleet():
    out = _run("avsync_venue_table").stdout.splitlines()
    assert out == [
        "snv|stream-snv|10.77.9.204|obs-e2e-test",
        "pp|stream-pp|stream-pp.lan|stream-pp",
    ]


def test_venue_names_in_table_order():
    assert _run("avsync_venue_names").stdout.split() == ["snv", "pp"]


def test_snv_rtmp_url_is_byte_identical_to_the_old_fixed_grab_url():
    # the URL scripts/avsync-measure-dev2.sh used to hard-code -- the SNV behaviour is unchanged.
    out = _run("avsync_venue_rtmp_url snv").stdout
    assert out == "rtmp://10.77.9.204:1234/live/obs-e2e-test"


def test_pp_rtmp_url_uses_the_fleet_row_host_and_the_box_name_key():
    # stream-pp publishes rtmp://127.0.0.1:1234/live/stream-pp on the box (issue 1436); dev2 pulls it
    # through the fleet row host, never the unreachable on-box STRIH_IP.
    out = _run("avsync_venue_rtmp_url pp").stdout
    assert out == "rtmp://stream-pp.lan:1234/live/stream-pp"
    assert "10.77.8.204" not in out


def test_box_host_and_ws_host_accessors():
    out = _run(
        'printf "%s %s %s\\n" "$(avsync_venue_box pp)" "$(avsync_venue_host pp)" "$(avsync_venue_ws_host pp)"\n'
        'printf "%s %s %s\\n" "$(avsync_venue_box snv)" "$(avsync_venue_host snv)" "$(avsync_venue_ws_host snv)"'
    ).stdout.splitlines()
    assert out == ["stream-pp stream-pp.lan stream-pp.lan", "stream-snv 10.77.9.204 10.77.9.204"]


def test_label_is_the_upper_case_venue():
    assert _run('avsync_venue_label pp; echo; avsync_venue_label snv').stdout.split() == ["PP", "SNV"]


def test_unknown_venue_fails_closed_with_a_named_error():
    p = _run("avsync_venue_rtmp_url zz", check=False)
    assert p.returncode != 0
    assert p.stdout == ""
    assert "zz" in p.stderr and "snv" in p.stderr and "pp" in p.stderr


def test_heartbeat_file_name_is_per_venue():
    out = _run('avsync_venue_heartbeat_file snv; echo; avsync_venue_heartbeat_file pp').stdout.split()
    assert out == ["avsync-watchdog-heartbeat-snv.txt", "avsync-watchdog-heartbeat-pp.txt"]


def test_legacy_heartbeat_venue_is_snv():
    # the venue whose old single heartbeat path stays a compatibility symlink (until the readers move)
    assert _run('printf "%s" "$AVSYNC_LEGACY_HEARTBEAT_VENUE"').stdout == "snv"


# ── derived from the fact files: a new stream box is a fact file + a fleet row, no code edit ─────


def _fleet(extra_rows):
    base = "stream-snv|10.77.9.204|windows-genlock|always\n"
    return base + extra_rows


def test_a_new_linux_stream_box_joins_from_its_fact_file(tmp_path):
    _write_box(tmp_path, "stream-ke", "stream", "ke")
    _write_box(tmp_path, "strih-ke", "strih", "ke")      # not a stream box: never a venue
    out = _run(
        "avsync_venue_table",
        {
            "STRIH_BOXES_DIR": str(tmp_path),
            "OBS_FLEET": _fleet("stream-ke|stream-ke.lan|linux-genlock|traveling"),
        },
    ).stdout.splitlines()
    assert out == [
        "snv|stream-snv|10.77.9.204|obs-e2e-test",
        "ke|stream-ke|stream-ke.lan|stream-ke",
    ]


def test_a_stream_box_without_a_fleet_row_is_skipped_loudly(tmp_path):
    _write_box(tmp_path, "stream-ke", "stream", "ke")
    p = _run(
        "avsync_venue_table",
        {"STRIH_BOXES_DIR": str(tmp_path), "OBS_FLEET": _fleet("")},
    )
    assert p.stdout.splitlines() == ["snv|stream-snv|10.77.9.204|obs-e2e-test"]
    assert "stream-ke" in p.stderr and "fleet" in p.stderr


def test_a_fact_file_venue_wins_over_the_declared_windows_row(tmp_path):
    # when stream-snv moves to the Linux stream role it gets a fact file: that row wins (key = box
    # name), and the stale declared row is named on stderr.
    _write_box(tmp_path, "stream-snv", "stream", "snv")
    p = _run("avsync_venue_table", {"STRIH_BOXES_DIR": str(tmp_path)})
    assert p.stdout.splitlines() == ["snv|stream-snv|10.77.9.204|stream-snv"]
    assert "declared" in p.stderr


def test_an_unparseable_fact_file_is_skipped_never_executed(tmp_path):
    (tmp_path / "stream-xx.env").write_text("STRIH_ROLE=stream\n$(touch INJECTED)\n")
    p = _run("avsync_venue_table", {"STRIH_BOXES_DIR": str(tmp_path)}, check=False)
    assert p.returncode == 0
    assert p.stdout.splitlines() == ["snv|stream-snv|10.77.9.204|obs-e2e-test"]
    assert "stream-xx" in p.stderr
    assert not (tmp_path / "INJECTED").exists()
    assert not pathlib.Path("INJECTED").exists()


def test_a_bad_venue_name_in_a_fact_file_is_refused(tmp_path):
    _write_box(tmp_path, "stream-ke", "stream", "Ke/../x")
    p = _run(
        "avsync_venue_table",
        {
            "STRIH_BOXES_DIR": str(tmp_path),
            "OBS_FLEET": _fleet("stream-ke|stream-ke.lan|linux-genlock|traveling"),
        },
    )
    assert p.stdout.splitlines() == ["snv|stream-snv|10.77.9.204|obs-e2e-test"]
    assert "stream-ke" in p.stderr


# ── the whole-table override (tests / ops), validated row by row ─────────────────────────────────


def test_table_override_is_authoritative():
    out = _run(
        "avsync_venue_table; avsync_venue_rtmp_url ab",
        {"AVSYNC_VENUE_TABLE": "ab|stream-ab|192.0.2.7|key-ab\n"},
    ).stdout.splitlines()
    assert out == ["ab|stream-ab|192.0.2.7|key-ab", "rtmp://192.0.2.7:1234/live/key-ab"]


def test_table_override_drops_a_malformed_row_loudly():
    p = _run(
        "avsync_venue_table",
        {"AVSYNC_VENUE_TABLE": "ab|stream-ab|192.0.2.7|key-ab\ncd|-x|h|k\nef|stream-ef|bad host|k\n"},
    )
    assert p.stdout.splitlines() == ["ab|stream-ab|192.0.2.7|key-ab"]
    assert "cd" in p.stderr and "ef" in p.stderr


def test_rtmp_port_and_app_are_the_restreamer_convention_constants():
    out = _run('printf "%s %s" "$AVSYNC_RTMP_PORT" "$AVSYNC_RTMP_APP"').stdout
    assert out == "1234 live"


# ── the dev1 venue selection: AVSYNC_VENUES narrows, an unknown name is refused ──────────────────


def test_selected_venues_default_is_every_table_venue():
    assert _run("avsync_selected_venues").stdout.split() == ["snv", "pp"]


def test_selected_venues_env_narrows_in_table_order():
    assert _run("avsync_selected_venues", {"AVSYNC_VENUES": "pp snv"}).stdout.split() == ["snv", "pp"]
    assert _run("avsync_selected_venues", {"AVSYNC_VENUES": "pp"}).stdout.split() == ["pp"]


def test_selected_venues_names_an_unknown_venue_on_stderr():
    p = _run("avsync_selected_venues", {"AVSYNC_VENUES": "snv qq"})
    assert p.stdout.split() == ["snv"]
    assert "qq" in p.stderr


def test_sourcing_runs_nothing_and_leaks_no_errexit_failure():
    # sourced under set -euo pipefail and immediately exits 0: nothing at source time can fail.
    assert _run("echo sourced").stdout == "sourced\n"
