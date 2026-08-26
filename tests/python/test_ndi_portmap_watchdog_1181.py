"""#1181 — the dev1-side NDI sender port-map stability watchdog:
`scripts/lib/ndi-portmap-health.sh` (pure map-diff), `scripts/ndi-portmap-audit.sh` (--capture/
--check/--json), and `scripts/ndi-portmap-alert-watchdog.sh` (confirm/throttle → one Discord alert).

Tier-0 tests: they shell out to bash (no cargo, no rig, no avahi — every avahi read is fed from a
fixture via NDI_PORTMAP_AVAHI_FIXTURE) to pin the PURE parse+diff, the audit CLI's contract, and the
watchdog's ships-DISABLED / shared-decision-lib convention. Under Local Build Policy Tier-0 (#557) no
cargo compiles here, so the port-map logic is proven entirely at the bash level.

The port-reshuffle hazard (evidence #1180/#1181): libndi assigns sender TCP ports sequentially from
5961 in creation order in one OBS process; DistroAV defers the main/preview outputs to
OBS_FRONTEND_EVENT_FINISHED_LOADING (after the ndi_filter republishes at scene-collection load), so a
live add/remove reshuffles the map on the next restart and a stock receiver on a cached port silently
shows the wrong sender.
"""
import json
import pathlib
import subprocess
import tempfile

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "ndi-portmap-health.sh"
_AUDIT = _ROOT / "scripts" / "ndi-portmap-audit.sh"
_WATCHDOG = _ROOT / "scripts" / "ndi-portmap-alert-watchdog.sh"
_BASELINE = _ROOT / "scripts" / "ndi-portmap-baseline.json"
_SVC = _ROOT / "systemd" / "ndi-portmap-alert-watchdog.service"
_TIMER = _ROOT / "systemd" / "ndi-portmap-alert-watchdog.timer"

# A live-shaped avahi -p resolved dump: the strih box (10.77.9.202) advertises the OBS instance
# (mDNS host 0000211c-c948: PGM/PVW/Grading/MULTIVIEW/interkom) AND a SEPARATE Arena/CG-bridge Spout
# (host 00001550-3342: "Arena - bible", :5961), plus unrelated CAM/STREAM sources. \NNN are avahi's
# DECIMAL escapes (\032=space, \040="(", \041=")").
_H_OBS = "STRIH-SNV-0000211c-c948.local"
_H_CG = "STRIH-SNV-00001550-3342.local"
_AVAHI_LIVE = "\n".join([
    "+;enp2s0;IPv4;STRIH-SNV\\032\\0402ME\\032PGM\\041;_ndi._tcp;local",  # unresolved browse line
    f"=;enp2s0;IPv4;STRIH-SNV\\032\\040Arena\\032-\\032bible\\041;_ndi._tcp;local;{_H_CG};10.77.9.202;5961;\"g=P\"",
    f"=;enp2s0;IPv4;STRIH-SNV\\032\\0402ME\\032PVW\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5966;\"g=P\"",
    f"=;enp2s0;IPv4;STRIH-SNV\\032\\040Grading\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5964;\"g=P\"",
    f"=;enp2s0;IPv4;STRIH-SNV\\032\\040MULTIVIEW\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5963;\"g=P\"",
    f"=;enp2s0;IPv4;STRIH-SNV\\032\\040interkom\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5962;\"g=P\"",
    "=;enp2s0;IPv4;CAM1\\032\\040usb\\041;_ndi._tcp;local;CAM1.local;10.77.9.61;5961;\"g=p\"",
    f"=;enp2s0;IPv4;STRIH-SNV\\032\\0402ME\\032PGM\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5965;\"g=P\"",
    "=;enp2s0;IPv4;STREAM-SNV\\032\\040stream\\041;_ndi._tcp;local;STREAM-SNV-0.local;10.77.9.204;5961;\"g=P\"",
]) + "\n"


def _bash(snippet):
    r = subprocess.run(["bash", "-c", f'. "{_LIB}"; {snippet}'], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _write(dirpath, name, content):
    p = pathlib.Path(dirpath) / name
    p.write_text(content)
    return str(p)


def _audit(mode, avahi_text, baseline_path, extra_env=None):
    with tempfile.TemporaryDirectory() as d:
        fix = _write(d, "avahi.txt", avahi_text)
        env = {"NDI_PORTMAP_AVAHI_FIXTURE": fix, "NDI_PORTMAP_BASELINE": baseline_path,
               "PATH": "/usr/bin:/bin"}
        if extra_env:
            env.update(extra_env)
        return subprocess.run(["bash", str(_AUDIT), mode], capture_output=True, text=True, env=env)


# ------------------------------------------------------------------ pure lib

def test_unescape_decodes_decimal_avahi_escapes():
    assert _bash('ndi_avahi_unescape "STRIH-SNV\\032\\0402ME\\032PGM\\041"') == "STRIH-SNV (2ME PGM)"
    assert _bash('ndi_avahi_unescape "STRIH-SNV\\032\\040Arena\\032-\\032bible\\041"') == "STRIH-SNV (Arena - bible)"
    # a backslash NOT followed by 3 digits passes through; a plain name is unchanged.
    assert _bash('ndi_avahi_unescape "plain"') == "plain"
    assert _bash('ndi_avahi_unescape "a\\\\b"') == "a\\b"


def test_parse_resolved_extracts_name_ip_port_host():
    line = (f'=;enp2s0;IPv4;STRIH-SNV\\032\\0402ME\\032PGM\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5965;"g=P"')
    out = _bash(f'ndi_avahi_parse_resolved {json.dumps(line)}').rstrip("\n")
    assert out == f"STRIH-SNV (2ME PGM)\t10.77.9.202\t5965\t{_H_OBS}"


def test_parse_resolved_ignores_browse_lines_and_bad_ports():
    assert _bash('ndi_avahi_parse_resolved "+;enp2s0;IPv4;X;_ndi._tcp;local"') == ""
    bad = '=;enp2s0;IPv4;X;_ndi._tcp;local;h.local;10.0.0.1;notaport;"g=P"'
    assert _bash(f'ndi_avahi_parse_resolved {json.dumps(bad)}') == ""


def test_select_isolates_the_obs_instance_and_excludes_cg_and_other_boxes():
    # build the tsv block from every resolved line, then select the OBS instance group.
    build = (
        'block=""; while IFS= read -r l; do p="$(ndi_avahi_parse_resolved "$l")"; '
        '[ -n "$p" ] && block="${block}${p}"$\'\\n\'; done <<EOF\n' + _AVAHI_LIVE + 'EOF\n'
        'ndi_portmap_select "$block" "10.77.9.202" "STRIH-SNV" "STRIH-SNV (2ME PGM)"'
    )
    out = _bash(build).strip().splitlines()
    got = dict(x.rsplit("=", 1) for x in out)
    assert got == {
        "STRIH-SNV (2ME PGM)": "5965", "STRIH-SNV (2ME PVW)": "5966",
        "STRIH-SNV (Grading)": "5964", "STRIH-SNV (MULTIVIEW)": "5963",
        "STRIH-SNV (interkom)": "5962",
    }
    # the Arena/CG-bridge Spout (:5961, different mDNS host) is EXCLUDED; so are CAM/STREAM.
    assert "STRIH-SNV (Arena - bible)" not in got
    assert not any(k.startswith("CAM") or k.startswith("STREAM") for k in got)


def test_select_emits_nothing_when_the_anchor_is_absent():
    build = (
        'block=""; while IFS= read -r l; do p="$(ndi_avahi_parse_resolved "$l")"; '
        '[ -n "$p" ] && block="${block}${p}"$\'\\n\'; done <<EOF\n' + _AVAHI_LIVE + 'EOF\n'
        'ndi_portmap_select "$block" "10.77.9.202" "STRIH-SNV" "STRIH-SNV (NO SUCH)"'
    )
    assert _bash(build).strip() == ""


def test_select_bails_when_anchor_seen_under_two_hostnames():
    # if the anchor name appears under >1 mDNS hostname (an ambiguous OBS-instance pick), select must
    # fail SAFE and emit NOTHING (an empty/gather-error map never pages, far better than silently
    # picking the wrong group the whole tool rests on).
    dup = _AVAHI_LIVE + (
        '=;enp2s0;IPv4;STRIH-SNV\\032\\0402ME\\032PGM\\041;_ndi._tcp;local;'
        'STRIH-SNV-DEAD-BEEF.local;10.77.9.202;5999;"g=P"\n')
    build = (
        'block=""; while IFS= read -r l; do p="$(ndi_avahi_parse_resolved "$l")"; '
        '[ -n "$p" ] && block="${block}${p}"$\'\\n\'; done <<EOF\n' + dup + 'EOF\n'
        'ndi_portmap_select "$block" "10.77.9.202" "STRIH-SNV" "STRIH-SNV (2ME PGM)"'
    )
    assert _bash(build).strip() == ""


def test_select_dedupes_a_doubled_resolve():
    # a multi-homed avahi resolve emitting the SAME name=port line twice must not double-report it.
    dupline = (f'=;wlan0;IPv4;STRIH-SNV\\032\\040interkom\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5962;"g=P"')
    build = (
        'block=""; while IFS= read -r l; do p="$(ndi_avahi_parse_resolved "$l")"; '
        '[ -n "$p" ] && block="${block}${p}"$\'\\n\'; done <<EOF\n' + _AVAHI_LIVE + dupline + '\n' + 'EOF\n'
        'ndi_portmap_select "$block" "10.77.9.202" "STRIH-SNV" "STRIH-SNV (2ME PGM)"'
    )
    out = _bash(build).strip().splitlines()
    assert out.count("STRIH-SNV (interkom)=5962") == 1
    assert len(out) == 5


def test_classify_port_ok_moved_absent_unset():
    assert _bash('ndi_portmap_classify_port 5965 5965').strip() == "OK"
    assert _bash('ndi_portmap_classify_port 5966 5965').strip() == "MOVED"
    assert _bash('ndi_portmap_classify_port "" 5965').strip() == "ABSENT"
    assert _bash('ndi_portmap_classify_port 5965 ""').strip() == "UNSET"


def test_verdict_changed_only_on_moved():
    assert _bash('ndi_portmap_verdict OK OK ABSENT').strip() == "STABLE"
    assert _bash('ndi_portmap_verdict OK MOVED OK').strip() == "CHANGED"
    assert _bash('ndi_portmap_verdict').strip() == "STABLE"


def test_lib_is_source_only_running_it_defines_but_does_nothing():
    r = subprocess.run(["bash", str(_LIB)], capture_output=True, text=True)
    assert r.returncode == 0 and r.stdout == "" and r.stderr == ""


# ------------------------------------------------------------------ audit CLI

def test_audit_syntax_valid():
    r = subprocess.run(["bash", "-n", str(_AUDIT)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def test_audit_help_exits_0_and_never_leaks_the_set_line():
    r = subprocess.run(["bash", str(_AUDIT), "--help"], capture_output=True, text=True)
    assert r.returncode == 0
    out = r.stdout.lower()
    assert "reshuffle" in out and "avahi" in out and "baseline" in out
    # --help must print ONLY the comment header, never leak the `set -uo pipefail` code line.
    assert "set -uo pipefail" not in r.stdout


def test_audit_rejects_unknown_arg():
    r = subprocess.run(["bash", str(_AUDIT), "--nope"], capture_output=True, text=True)
    assert r.returncode == 2


def test_audit_json_isolates_the_obs_instance():
    r = _audit("--json", _AVAHI_LIVE, "/nonexistent/baseline.json")
    assert r.returncode == 0, r.stderr
    data = json.loads(r.stdout)
    assert data["senders"] == {
        "STRIH-SNV (2ME PGM)": 5965, "STRIH-SNV (2ME PVW)": 5966,
        "STRIH-SNV (Grading)": 5964, "STRIH-SNV (MULTIVIEW)": 5963,
        "STRIH-SNV (interkom)": 5962,
    }


def test_audit_capture_then_check_is_stable():
    with tempfile.TemporaryDirectory() as d:
        base = str(pathlib.Path(d) / "baseline.json")
        r = _audit("--capture", _AVAHI_LIVE, base)
        assert r.returncode == 0, r.stderr
        j = json.loads(pathlib.Path(base).read_text())
        assert j["anchor"] == "STRIH-SNV (2ME PGM)" and j["ip"] == "10.77.9.202"
        assert j["senders"]["STRIH-SNV (2ME PGM)"] == 5965
        r = _audit("--check", _AVAHI_LIVE, base)
        assert r.returncode == 0, r.stderr
        assert "NDI-PORTMAP-STABLE" in r.stdout


def test_audit_check_detects_a_moved_port_exit_3():
    with tempfile.TemporaryDirectory() as d:
        base = str(pathlib.Path(d) / "baseline.json")
        assert _audit("--capture", _AVAHI_LIVE, base).returncode == 0
        # PGM and PVW swap ports (the reshuffle signature).
        reshuffled = (_AVAHI_LIVE.replace(";10.77.9.202;5966;", ";10.77.9.202;__T__;")
                      .replace("STRIH-SNV\\032\\0402ME\\032PGM\\041;_ndi._tcp;local;" + _H_OBS + ";10.77.9.202;5965;",
                               "STRIH-SNV\\032\\0402ME\\032PGM\\041;_ndi._tcp;local;" + _H_OBS + ";10.77.9.202;5966;")
                      .replace(";10.77.9.202;__T__;", ";10.77.9.202;5965;"))
        r = _audit("--check", reshuffled, base)
        assert r.returncode == 3, (r.stdout, r.stderr)
        assert "NDI-PORTMAP-CHANGED" in r.stdout
        assert "2ME PGM" in r.stdout


def test_audit_check_empty_live_map_is_gather_error_not_a_change():
    # OBS down / avahi unreachable / anchor renamed -> empty live map -> exit 2 (never 3).
    with tempfile.TemporaryDirectory() as d:
        base = str(pathlib.Path(d) / "baseline.json")
        assert _audit("--capture", _AVAHI_LIVE, base).returncode == 0
        r = _audit("--check", "", base)
        assert r.returncode == 2, (r.stdout, r.stderr)


def test_audit_check_missing_anchor_is_gather_error_not_a_change():
    with tempfile.TemporaryDirectory() as d:
        base = str(pathlib.Path(d) / "baseline.json")
        assert _audit("--capture", _AVAHI_LIVE, base).returncode == 0
        no_anchor = _AVAHI_LIVE.replace("2ME\\032PGM", "2ME\\032NOPE")
        r = _audit("--check", no_anchor, base)
        assert r.returncode == 2, (r.stdout, r.stderr)


def test_audit_check_new_sender_is_report_only_not_a_page():
    with tempfile.TemporaryDirectory() as d:
        base = str(pathlib.Path(d) / "baseline.json")
        assert _audit("--capture", _AVAHI_LIVE, base).returncode == 0
        added = _AVAHI_LIVE + (
            f'=;enp2s0;IPv4;STRIH-SNV\\032\\040Aux\\041;_ndi._tcp;local;{_H_OBS};10.77.9.202;5967;"g=P"\n')
        r = _audit("--check", added, base)
        assert r.returncode == 0, (r.stdout, r.stderr)  # a NEW sender does not page
        assert "NEW" in r.stderr and "Aux" in r.stderr


def test_audit_check_report_only_absent_with_anchor_present_stays_stable():
    # a NON-anchor sender removed (anchor still present) -> [report-only ABSENT], still exit 0 (a
    # removed output reshuffles only the NEXT restart; it is not a live moved port).
    with tempfile.TemporaryDirectory() as d:
        base = str(pathlib.Path(d) / "baseline.json")
        assert _audit("--capture", _AVAHI_LIVE, base).returncode == 0
        no_interkom = "\n".join(l for l in _AVAHI_LIVE.splitlines() if "interkom" not in l) + "\n"
        r = _audit("--check", no_interkom, base)
        assert r.returncode == 0, (r.stdout, r.stderr)
        assert "ABSENT" in r.stderr and "interkom" in r.stderr
        assert "NDI-PORTMAP-STABLE" in r.stdout


def test_audit_capture_refuses_empty_avahi_writes_no_file():
    with tempfile.TemporaryDirectory() as d:
        base = str(pathlib.Path(d) / "baseline.json")
        r = _audit("--capture", "", base)
        assert r.returncode == 2, (r.stdout, r.stderr)
        assert not pathlib.Path(base).exists()  # a failed capture must never write a truncated baseline


def test_baseline_json_checked_in_and_captured_from_live():
    j = json.loads(_BASELINE.read_text())
    assert j["ip"] == "10.77.9.202" and j["anchor"] == "STRIH-SNV (2ME PGM)"
    # The checked-in baseline is RE-CAPTURED from the live rig whenever the operating port map
    # legitimately changes (the rule's own documented procedure), so the exact port VALUES are
    # ephemeral rig state. Pin the STRUCTURE, not the snapshot — the first documented re-capture
    # (2026-08-24, after a strih OBS restart reshuffle) broke the old exact-map assertion.
    senders = j["senders"]
    assert j["anchor"] in senders
    assert len(senders) >= 3
    assert all(isinstance(p, int) and 5961 <= p <= 6010 for p in senders.values())
    assert len(set(senders.values())) == len(senders)  # one distinct port per sender
    assert "never hand-typed" in j["_comment"]


# ------------------------------------------------------------------ alert watchdog

def _stub_audit(dirpath, rc, summary):
    # a fake ndi-portmap-audit.sh that echoes <summary> and exits <rc>, so the watchdog's
    # confirm/throttle/page flow can be exercised with no avahi and no rig.
    p = pathlib.Path(dirpath) / "stub-audit.sh"
    p.write_text(f'#!/usr/bin/env bash\nset -uo pipefail\necho {json.dumps(summary)}\nexit {rc}\n')
    p.chmod(0o755)
    return str(p)


def _run_watchdog(dirpath, audit_cmd, dry_run=True, prior_state=None):
    state = str(pathlib.Path(dirpath) / "wd.state")
    if prior_state is not None:
        pathlib.Path(state).write_text(prior_state)
    env = {"PATH": "/usr/bin:/bin", "HOME": dirpath,
           "NDI_PORTMAP_ALERT_AUDIT_CMD": audit_cmd,
           "NDI_PORTMAP_ALERT_STATE_FILE": state,
           "AIRULESET_NOTIFY": "/nonexistent/airuleset.py"}
    args = ["bash", str(_WATCHDOG)] + (["--dry-run"] if dry_run else [])
    r = subprocess.run(args, capture_output=True, text=True, env=env)
    return r, state


def test_watchdog_syntax_valid():
    r = subprocess.run(["bash", "-n", str(_WATCHDOG)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def test_watchdog_help_exits_0_and_never_leaks_the_set_line():
    r = subprocess.run(["bash", str(_WATCHDOG), "--help"], capture_output=True, text=True)
    assert r.returncode == 0
    out = r.stdout.lower()
    assert "reshuffle" in out and "studio monitor" in out
    assert "set -uo pipefail" not in r.stdout


def test_watchdog_rejects_unknown_arg():
    r = subprocess.run(["bash", str(_WATCHDOG), "--nope"], capture_output=True, text=True)
    assert r.returncode == 2


def test_watchdog_reuses_the_shared_obs_watchdog_decision_lib_not_a_second_mechanism():
    body = _WATCHDOG.read_text()
    assert "obs-watchdog-decision.sh" in body
    assert "obs_watchdog_confirm" in body and "obs_watchdog_alert_throttle" in body
    assert "notify" in body and "airuleset" in body.lower()


def test_watchdog_alert_body_is_slovak_and_owner_actionable():
    body = _WATCHDOG.read_text()
    # #1117: owner-facing alerts must be Slovak; must name the operator action.
    assert "NESPRÁVNY zdroj" in body and "prijímače" in body
    assert "--capture" in body  # the re-capture action


def test_watchdog_confirms_across_two_passes_before_paging():
    with tempfile.TemporaryDirectory() as d:
        audit = _stub_audit(d, 3, "NDI-PORTMAP-CHANGED: 1 sender(s) moved: STRIH-SNV (2ME PGM) :5965->:5966")
        # pass 1: CHANGED but not yet confirmed (threshold 2) -> holds, no alert.
        r1, state = _run_watchdog(d, audit, dry_run=True)
        assert r1.returncode == 0
        assert "not yet CONFIRMED" in r1.stderr
        # pass 2 (reusing the persisted confirm counter): now confirmed -> WOULD alert.
        r2, _ = _run_watchdog(d, audit, dry_run=True,
                              prior_state=pathlib.Path(state).read_text())
        assert "WOULD alert" in r2.stderr


def test_watchdog_gather_error_never_pages():
    # exit 2 (OBS down / avahi unreachable / anchor absent) is "nothing to decide", never a page.
    with tempfile.TemporaryDirectory() as d:
        audit = _stub_audit(d, 2, "gather error")
        r, _ = _run_watchdog(d, audit, dry_run=True)
        assert r.returncode == 0
        assert "nothing to decide" in r.stderr
        assert "WOULD alert" not in r.stderr


def test_watchdog_stable_clears_and_recovers():
    with tempfile.TemporaryDirectory() as d:
        audit = _stub_audit(d, 0, "NDI-PORTMAP-STABLE: OBS instance port map matches baseline (5 senders)")
        # a prior state that had already alerted -> a STABLE pass fires the recovery note.
        r, _ = _run_watchdog(d, audit, dry_run=True, prior_state="alerted=1\nconfirm=2\n")
        assert r.returncode == 0
        assert "WOULD send recovery" in r.stderr


def test_systemd_units_present_and_disabled_by_default():
    svc = _SVC.read_text()
    tmr = _TIMER.read_text()
    assert "Type=oneshot" in svc
    assert "ndi-portmap-alert-watchdog.sh" in svc
    assert "WantedBy=timers.target" in tmr and "OnUnitActiveSec=" in tmr
    # ships DISABLED: no installer enables these units (setup-device/build-image do not reference them).
    for installer in ("setup-device.sh", "build-image.sh"):
        p = _ROOT / "scripts" / installer
        if p.exists():
            assert "ndi-portmap-alert-watchdog" not in p.read_text()
