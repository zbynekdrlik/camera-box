"""issue 1242 -- the cambox NDI egress pacing is PERMANENT: provisioned, boot-applied, verified.

The owner approved staggered camera sending (issue comment 5904641502). Delaying the frame hand-off
failed live twice (the NDI SDK encodes inside the send), so the bursts are spread by pacing each
cambox's kernel egress instead: `tc qdisc replace dev <default-route if> root fq maxrate 400mbit
flow_limit 2000 limit 20000` (decision 5904673375, result 5905948208). Until now it lived only as a
runtime qdisc applied by hand; a reboot, a re-provision or a new stick dropped it silently.

The main-authored design (5905959484) pinned here:

  * ONE declaration -- scripts/lib/cambox-egress-pacing.sh: the rate / flow_limit / limit and the
    pure builders + verdicts; nothing else in scripts/ or systemd/ spells the rate;
  * the apply command resolves the default-route interface at RUN time (`ip route show default`,
    first = lowest-metric route; enp/enx names alike) and fails loud with no route;
  * the verdict parses `tc qdisc show dev <if>` -> OK / DRIFT / UNKNOWN (real cam7 output below);
  * the generated on-box script retries the apply for a bounded ~60 s (a DHCP-late default route),
    then fails loud, never silent; the checked-in systemd/cambox-egress-pacing.service runs it at
    boot (oneshot, After/Wants network-online.target, RemainAfterExit);
  * setup-device.sh installs + enables it, enable-only (never a live start, never a live tc apply),
    in the rw window before STEP 18, and refuses the run before any write when systemd/ is missing;
  * verify-device.sh (ap) FAILs a box whose live qdisc drifted or whose unit is not enabled / failed,
    and sits before (q) outside the slices other pytests execute;
  * the E2E `[0/8]` reads every vetted cambox REPORT-ONLY: a missing pacing is a named WARNING and
    the run continues.

Offline (no box, no cargo): fake `ip` / `tc` / `systemctl` / `sshpass` on PATH.
"""
import hashlib
import os
import pathlib
import re
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
LIB = SCRIPTS / "lib" / "cambox-egress-pacing.sh"
UNIT = REPO / "systemd" / "cambox-egress-pacing.service"
SETUP_DEVICE = SCRIPTS / "setup-device.sh"
VERIFY_DEVICE = SCRIPTS / "verify-device.sh"
E2E = SCRIPTS / "recording-e2e.sh"

# Real `tc qdisc show dev <if>` output read live from cam7 on 30.9.2026 (the supervisor's read,
# issue 1242 dispatch): the paced root, and the unpaced default of a fresh boot.
CAM7_PACED = (
    "qdisc fq 8001: root refcnt 2 limit 20000p flow_limit 2000p buckets 1024 orphan_mask 1023 "
    "quantum 3028b initial_quantum 15140b maxrate 400Mbit low_rate_threshold 550Kbit "
    "refill_delay 40ms timer_slack 10us horizon 10s horizon_drop"
)
CAM7_UNPACED = "qdisc pfifo_fast 0: root refcnt 2 bands 3 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1"
# A plain `fq` with the kernel defaults (no maxrate) -- the qdisc is fq, but nothing is paced.
FQ_NO_MAXRATE = (
    "qdisc fq 8001: root refcnt 2 limit 10000p flow_limit 100p buckets 1024 orphan_mask 1023 "
    "quantum 3028b initial_quantum 15140b low_rate_threshold 550Kbit refill_delay 40ms "
    "timer_slack 10us horizon 10s horizon_drop"
)
# A multi-queue NIC's default: an `mq` root with per-queue children.
MQ_DEFAULT = (
    "qdisc mq 0: root \n"
    "qdisc pfifo_fast 0: parent :2 bands 3 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1\n"
    "qdisc pfifo_fast 0: parent :1 bands 3 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1"
)
ROUTE_ENP2 = "default via 10.77.8.1 dev enp2s0 proto static \n"


def _read(path):
    return pathlib.Path(path).read_text()


def _run(script, env=None, timeout=60):
    full = dict(os.environ)
    for k in list(full):
        if k.startswith(("CAMBOX_EGRESS_PACING_", "FAKE_")):
            del full[k]
    full.update(env or {})
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=full,
                          timeout=timeout)


def _lib(body, env=None):
    """Source the lib under the caller's strict mode, as setup-device / verify-device / the E2E do."""
    return _run(f'set -euo pipefail\n. "{LIB}"\n{body}', env=env)


def _fakes(tmp_path):
    """A bin dir with fake ip / tc / systemctl / sshpass, driven by FAKE_* env vars."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    (bindir / "ip").write_text(
        '#!/usr/bin/env bash\n'
        'n=0; [ -f "$FAKE_DIR/ip.calls" ] && n=$(cat "$FAKE_DIR/ip.calls")\n'
        'n=$((n + 1)); echo "$n" >"$FAKE_DIR/ip.calls"\n'
        'echo "$*" >>"$FAKE_DIR/ip.argv"\n'
        '[ "$n" -le "${FAKE_IP_EMPTY_FIRST:-0}" ] && exit 0\n'
        'printf "%s" "${FAKE_IP_ROUTE:-}"\n')
    (bindir / "tc").write_text(
        '#!/usr/bin/env bash\n'
        'echo "$*" >>"$FAKE_DIR/tc.argv"\n'
        'if [ "$1 $2" = "qdisc show" ]; then printf "%s\\n" "${FAKE_TC_SHOW:-}"; exit 0; fi\n'
        'exit "${FAKE_TC_RC:-0}"\n')
    (bindir / "systemctl").write_text(
        '#!/usr/bin/env bash\n'
        'echo "$*" >>"$FAKE_DIR/systemctl.argv"\n'
        'case "$1" in\n'
        '  is-enabled) echo "${FAKE_ENABLED:-enabled}" ;;\n'
        '  is-active) echo "${FAKE_ACTIVE:-active}" ;;\n'
        '  show) echo "${FAKE_RESTARTS:-0}" ;;\n'
        'esac\n')
    # sshpass -p PASS ssh ... USER@IP CMD -> print $FAKE_DIR/ssh/<ip>.txt, or fail like an
    # unreachable box (255) when there is none.
    (bindir / "sshpass").write_text(
        '#!/usr/bin/env bash\n'
        'echo "$*" >>"$FAKE_DIR/sshpass.argv"\n'
        'for a in "$@"; do case "$a" in *@*) host="${a#*@}" ;; esac; done\n'
        '[ -f "$FAKE_DIR/ssh/$host.txt" ] || exit 255\n'
        'cat "$FAKE_DIR/ssh/$host.txt"\n')
    for f in bindir.iterdir():
        f.chmod(0o755)
    (tmp_path / "ssh").mkdir()
    return {"PATH": f"{bindir}:{os.environ['PATH']}", "FAKE_DIR": str(tmp_path)}


def _argv(tmp_path, tool):
    p = tmp_path / f"{tool}.argv"
    return p.read_text().splitlines() if p.exists() else []


# =================================================================================================
# The ONE declaration
# =================================================================================================

def test_the_lib_is_the_one_declaration_of_rate_and_limits():
    r = _lib('echo "$CAMBOX_EGRESS_PACING_RATE $CAMBOX_EGRESS_PACING_FLOW_LIMIT '
             '$CAMBOX_EGRESS_PACING_LIMIT"')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["400mbit", "2000", "20000"]


def test_nothing_else_in_scripts_or_systemd_spells_the_pacing_rate():
    hits = []
    for base in (SCRIPTS, REPO / "systemd"):
        for p in base.rglob("*"):
            if p.is_file() and p != LIB and p.suffix in (".sh", ".py", ".service", ".ps1", ""):
                try:
                    text = p.read_text()
                except UnicodeDecodeError:
                    continue
                if re.search(r"maxrate\s+[0-9]", text) or "flow_limit 2000" in text:
                    hits.append(str(p.relative_to(REPO)))
    assert hits == [], f"the rate/limits are declared ONCE, in {LIB.name}: {hits}"


def test_the_lib_is_source_only_and_strict_mode_safe():
    text = _read(LIB)
    head = "\n".join(text.splitlines()[:6])
    assert "airuleset:script-ok" in head
    # Sourcing must never change the caller's shell options (the generated on-box script inside a
    # heredoc carries its own `set -euo pipefail`; that never runs at source time).
    r = _run(f'set +euo pipefail\n. "{LIB}"\necho "FLAGS=$-"\nshopt -qo pipefail || echo NOPIPEFAIL')
    assert r.returncode == 0, r.stderr
    flags = r.stdout.splitlines()[0]
    assert "e" not in flags.split("=", 1)[1] and "u" not in flags.split("=", 1)[1], flags
    assert "NOPIPEFAIL" in r.stdout
    r = _lib("echo SOURCED")
    assert r.returncode == 0 and r.stdout.strip() == "SOURCED", r.stderr


# =================================================================================================
# The apply command: the default-route interface resolved at run time
# =================================================================================================

def _apply(tmp_path, env):
    fk = _fakes(tmp_path)
    return _lib('set +e\neval "$(cambox_egress_pacing_apply_cmd)"\necho "RC=$?"', env={**fk, **env})


def test_apply_paces_the_default_route_interface(tmp_path):
    r = _apply(tmp_path, {"FAKE_IP_ROUTE": ROUTE_ENP2})
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert _argv(tmp_path, "tc") == [
        "qdisc replace dev enp2s0 root fq maxrate 400mbit flow_limit 2000 limit 20000"]
    assert "enp2s0" in r.stdout and "applied" in r.stdout
    assert _argv(tmp_path, "ip") == ["route show default"]


def test_apply_takes_the_first_default_route_the_lowest_metric_one(tmp_path):
    routes = ("default via 10.77.8.1 dev enp3s0 proto static metric 100 \n"
              "default via 169.254.0.1 dev enx0011223344 proto static metric 200 \n")
    r = _apply(tmp_path, {"FAKE_IP_ROUTE": routes})
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert _argv(tmp_path, "tc") == [
        "qdisc replace dev enp3s0 root fq maxrate 400mbit flow_limit 2000 limit 20000"]


def test_apply_follows_a_renamed_interface(tmp_path):
    r = _apply(tmp_path, {"FAKE_IP_ROUTE": "default via 10.77.8.1 dev enx5c857e3a01 \n"})
    assert "RC=0" in r.stdout, r.stdout + r.stderr
    assert _argv(tmp_path, "tc")[0].startswith("qdisc replace dev enx5c857e3a01 root fq ")


def test_apply_without_a_default_route_fails_loud_and_touches_nothing(tmp_path):
    r = _apply(tmp_path, {"FAKE_IP_ROUTE": ""})
    assert "RC=1" in r.stdout, r.stdout + r.stderr
    assert _argv(tmp_path, "tc") == []
    assert "no default route" in r.stderr


def test_apply_reports_a_failed_tc(tmp_path):
    r = _apply(tmp_path, {"FAKE_IP_ROUTE": ROUTE_ENP2, "FAKE_TC_RC": "2"})
    assert "RC=1" in r.stdout, r.stdout + r.stderr
    assert "FAILED" in r.stderr


# =================================================================================================
# The verdict: `tc qdisc show dev <if>` -> OK / DRIFT / UNKNOWN
# =================================================================================================

def _verdict(text, pre=""):
    r = _lib(f'{pre}\ncambox_egress_pacing_verdict "$T"; echo "RC=$?"', env={"T": text})
    assert r.returncode == 0 and "RC=0" in r.stdout, r.stdout + r.stderr
    return r.stdout.splitlines()[0]


def test_verdict_ok_on_the_real_cam7_paced_root():
    v = _verdict(CAM7_PACED)
    assert v.startswith("OK "), v
    assert "400Mbit" in v


def test_verdict_ok_on_the_pipe_flattened_gather_form():
    assert _verdict(CAM7_PACED + "|").startswith("OK "), "the gather flattens newlines to |"


def test_verdict_drift_on_the_real_unpaced_default():
    v = _verdict(CAM7_UNPACED)
    assert v.startswith("DRIFT "), v
    assert "pfifo_fast" in v


def test_verdict_drift_on_fq_without_maxrate():
    v = _verdict(FQ_NO_MAXRATE)
    assert v.startswith("DRIFT ") and "maxrate" in v, v


def test_verdict_drift_on_a_wrong_rate_or_limits():
    for text, word in ((CAM7_PACED.replace("400Mbit", "300Mbit"), "maxrate"),
                       (CAM7_PACED.replace("flow_limit 2000p", "flow_limit 100p"), "flow_limit"),
                       (CAM7_PACED.replace("limit 20000p", "limit 10000p"), "limit 10000")):
        v = _verdict(text)
        assert v.startswith("DRIFT ") and word in v, (text, v)


def test_verdict_drift_on_a_multiqueue_default_root():
    v = _verdict(MQ_DEFAULT)
    assert v.startswith("DRIFT ") and "mq" in v, v


def test_verdict_unknown_when_unreadable():
    for text in ("", "__TC_ABSENT__", "__NO_DEFAULT_ROUTE__", "Cannot find device \"enp9s0\""):
        v = _verdict(text)
        assert v.startswith("UNKNOWN "), (text, v)


def test_verdict_compares_the_rate_in_bits_per_second():
    # tc renders the rate with its own unit: a 1gbit declaration reads back as 1Gbit.
    v = _verdict(CAM7_PACED.replace("400Mbit", "1Gbit"), pre="CAMBOX_EGRESS_PACING_RATE=1gbit")
    assert v.startswith("OK "), v
    r = _lib('for x in 400mbit 400Mbit 1Gbit 550Kbit 50mbps nonsense; do '
             'printf "%s=%s\\n" "$x" "$(cambox_egress_pacing_rate_bps "$x")"; done')
    assert r.stdout.split() == ["400mbit=400000000", "400Mbit=400000000", "1Gbit=1000000000",
                                "550Kbit=550000", "50mbps=400000000", "nonsense="], r.stdout


# =================================================================================================
# The gather snippet + the provisioning verdict (verify-device (ap), the E2E row)
# =================================================================================================

def _gather(tmp_path, env):
    fk = _fakes(tmp_path)
    return _lib('bash -c "$(cambox_egress_pacing_gather_remote_snippet)"', env={**fk, **env})


def test_gather_reads_the_default_route_qdisc_and_the_unit_state(tmp_path):
    r = _gather(tmp_path, {"FAKE_IP_ROUTE": ROUTE_ENP2, "FAKE_TC_SHOW": CAM7_PACED,
                           "FAKE_ENABLED": "enabled", "FAKE_ACTIVE": "inactive"})
    assert r.returncode == 0, r.stderr
    lines = r.stdout.splitlines()
    assert "PACING_IFACE=enp2s0" in lines
    assert f"PACING_QDISC={CAM7_PACED}|" in lines
    assert "PACING_SVC_ENABLED=enabled" in lines and "PACING_SVC_ACTIVE=inactive" in lines
    assert _argv(tmp_path, "tc") == ["qdisc show dev enp2s0"], "read-only: never a replace"


def test_gather_names_a_missing_default_route(tmp_path):
    r = _gather(tmp_path, {"FAKE_IP_ROUTE": ""})
    assert "PACING_QDISC=__NO_DEFAULT_ROUTE__" in r.stdout.splitlines(), r.stdout
    assert _argv(tmp_path, "tc") == []


def test_gather_reads_the_installed_script_and_unit(tmp_path):
    # Review round 1: (ap) must prove the unit can RUN, so the gather also reads the installed boot
    # script's executable bit + sha256 and the installed unit's sha256.
    fk = _fakes(tmp_path)
    script = tmp_path / "installed-script"
    script.write_text("#!/bin/bash\necho paced\n")
    script.chmod(0o755)
    unit = tmp_path / "installed.service"
    unit.write_text("[Service]\nType=oneshot\n")
    pre = (f'CAMBOX_EGRESS_PACING_SCRIPT_PATH="{script}"\n'
           f'CAMBOX_EGRESS_PACING_SERVICE_PATH="{unit}"\n')
    env = {**fk, "FAKE_IP_ROUTE": ROUTE_ENP2, "FAKE_TC_SHOW": CAM7_PACED, "FAKE_RESTARTS": "3"}
    lines = _lib(pre + 'bash -c "$(cambox_egress_pacing_gather_remote_snippet)"', env=env).stdout.splitlines()
    assert "PACING_SCRIPT_EXEC=yes" in lines, lines
    assert f"PACING_SCRIPT_SHA={_functional_sha(script.read_text())}" in lines, lines
    assert f"PACING_UNIT_SHA={_functional_sha(unit.read_text())}" in lines, lines
    assert "PACING_SVC_RESTARTS=3" in lines, lines
    # Executable but without its #!/bin/bash line is not runnable by systemd either.
    script.write_text("echo paced\n")
    lines = _lib(pre + 'bash -c "$(cambox_egress_pacing_gather_remote_snippet)"', env=env).stdout.splitlines()
    assert "PACING_SCRIPT_EXEC=no" in lines, lines
    script.chmod(0o644)
    unit.unlink()
    lines = _lib(pre + 'bash -c "$(cambox_egress_pacing_gather_remote_snippet)"', env=env).stdout.splitlines()
    assert "PACING_SCRIPT_EXEC=no" in lines and "PACING_UNIT_SHA=" in lines, lines


def _functional_sha(text):
    """sha256 of the FUNCTIONAL lines only: comment lines (leading `#`, incl. the shebang) and blank
    lines dropped -- review round 2, a comment-only edit must not turn every box stale."""
    keep = [ln for ln in text.splitlines(keepends=True)
            if not re.match(r"^[ \t]*#", ln) and not re.match(r"^[ \t]*$", ln)]
    return hashlib.sha256("".join(keep).encode()).hexdigest()


_EXPECTED = {}


def _expected():
    """The hashes setup-device's install would leave on a box: the generated boot script and the
    checked-in unit (computed by the lib's own helpers, cached)."""
    if not _EXPECTED:
        _EXPECTED["script"] = _lib("cambox_egress_pacing_expected_script_sha").stdout.strip()
        _EXPECTED["unit"] = _lib("cambox_egress_pacing_expected_unit_sha").stdout.strip()
    return _EXPECTED


def _block(qdisc=CAM7_PACED + "|", enabled="enabled", active="active", iface="enp3s0",
           script_exec="yes", script_sha=None, unit_sha=None, restarts="0"):
    e = _expected()
    script_sha = e["script"] if script_sha is None else script_sha
    unit_sha = e["unit"] if unit_sha is None else unit_sha
    return (f"PACING_IFACE={iface}\nPACING_QDISC={qdisc}\n"
            f"PACING_SVC_ENABLED={enabled}\nPACING_SVC_ACTIVE={active}\n"
            f"PACING_SVC_RESTARTS={restarts}\n"
            f"PACING_SCRIPT_EXEC={script_exec}\nPACING_SCRIPT_SHA={script_sha}\n"
            f"PACING_UNIT_SHA={unit_sha}\n")


def test_expected_hashes_are_the_functional_lines_setup_device_installs():
    boot = _lib("cambox_egress_pacing_boot_script").stdout
    assert _expected()["script"] == _functional_sha(boot)
    assert _expected()["unit"] == _functional_sha(UNIT.read_text())


def test_a_comment_only_edit_keeps_every_box_current(tmp_path):
    # Review round 2: the installed copies are compared on their functional lines, so a comment or
    # blank-line edit to the unit or the boot-script template needs no fleet re-provision; a
    # functional edit (another rate, another ExecStart) is still stale.
    fk = _fakes(tmp_path)
    unit = tmp_path / "installed.service"
    script = tmp_path / "installed-script"
    boot = _lib("cambox_egress_pacing_boot_script").stdout
    pre = (f'CAMBOX_EGRESS_PACING_SCRIPT_PATH="{script}"\n'
           f'CAMBOX_EGRESS_PACING_SERVICE_PATH="{unit}"\n')
    env = {**fk, "FAKE_IP_ROUTE": ROUTE_ENP2, "FAKE_TC_SHOW": CAM7_PACED}

    def grade(unit_text, script_text):
        unit.write_text(unit_text)
        script.write_text(script_text)
        script.chmod(0o755)
        block = _lib(pre + 'bash -c "$(cambox_egress_pacing_gather_remote_snippet)"', env=env).stdout
        return _prov(block)

    assert grade(_read(UNIT), boot) == "ok"
    assert grade("# a new comment\n\n" + _read(UNIT), boot.replace("\nset -euo", "\n# note\n\nset -euo")) == "ok"
    assert "differs" in grade(_read(UNIT).replace("RestartSec=30", "RestartSec=31"), boot)
    assert "stale" in grade(_read(UNIT), boot.replace("maxrate 400mbit", "maxrate 300mbit"))


def _prov(block):
    r = _lib('cambox_egress_pacing_provision_verdict "$B"; echo "RC=$?"', env={"B": block})
    assert r.returncode == 0 and "RC=0" in r.stdout, r.stdout + r.stderr
    return r.stdout.replace("RC=0", "").strip()


def test_provision_verdict_ok_when_paced_and_enabled():
    assert _prov(_block()) == "ok"
    # setup-device is enable-only and a cambox is never rebooted remotely: an enabled, not yet
    # started unit with the pacing live is a correctly provisioned box.
    assert _prov(_block(active="inactive")) == "ok"


def test_provision_verdict_fails_each_facet():
    assert "DRIFT" in _prov(_block(qdisc=CAM7_UNPACED + "|"))
    assert "not enabled" in _prov(_block(enabled="disabled"))
    assert "not enabled" in _prov(_block(enabled=""))
    assert "failed" in _prov(_block(active="failed"))
    assert "UNKNOWN" in _prov(_block(qdisc="__NO_DEFAULT_ROUTE__"))
    assert "UNKNOWN" in _prov("")
    for bad in (_block(qdisc=CAM7_UNPACED + "|"), _block(enabled="disabled"), ""):
        assert all(ln.startswith("FAIL: ") for ln in _prov(bad).splitlines()), _prov(bad)


def test_provision_verdict_fails_a_box_whose_unit_cannot_run():
    # Review round 1: enabled + not failed is not enough -- a missing, non-executable or stale boot
    # script (the rate changed in the lib, the box never re-provisioned) or a wrong unit would only
    # fail at the next reboot. Each is a FAIL now, pointing at setup-device.
    v = _prov(_block(script_exec="no"))
    assert "missing or not executable" in v and "setup-device" in v, v
    v = _prov(_block(script_sha="0" * 64))
    assert "stale" in v and "setup-device" in v, v
    v = _prov(_block(unit_sha="0" * 64))
    assert "unit" in v and "differs" in v and "setup-device" in v, v
    v = _prov(_block(unit_sha=""))
    assert "unit" in v and "setup-device" in v, v
    # The expected values come from the caller when given (the E2E computes them once per run).
    ok_block = _block(script_sha="a" * 64, unit_sha="b" * 64)
    r = _lib('cambox_egress_pacing_provision_verdict "$B" "$WS" "$WU"',
             env={"B": ok_block, "WS": "a" * 64, "WU": "b" * 64})
    assert r.stdout.strip() == "ok", r.stdout + r.stderr


def test_provision_verdict_gives_the_fix_that_matches_the_state():
    # Review round 1: never a blanket "re-run setup-device" when the install is fine.
    # Installed + enabled, never run since install, the runtime qdisc lost: start it.
    v = _prov(_block(qdisc=CAM7_UNPACED + "|", active="inactive"))
    assert "systemctl start cambox-egress-pacing" in v and "setup-device" not in v, v
    # It ran, then something changed the qdisc: restart re-applies it.
    v = _prov(_block(qdisc=CAM7_UNPACED + "|", active="active"))
    assert "systemctl restart cambox-egress-pacing" in v and "setup-device" not in v, v
    # No default route (the unit keeps retrying): a network problem, not provisioning.
    v = _prov(_block(qdisc="__NO_DEFAULT_ROUTE__", active="activating"))
    assert "network" in v and "setup-device" not in v, v
    # A failed unit points at its own journal; a missing tc at iproute2.
    assert "journalctl -u cambox-egress-pacing" in _prov(_block(qdisc=CAM7_UNPACED + "|", active="failed"))
    assert "iproute2" in _prov(_block(qdisc="__TC_ABSENT__"))


def test_provision_verdict_reads_a_retrying_unit_right():
    # Review round 2: with Restart=on-failure forever a unit whose tc keeps failing (a kernel without
    # sch_fq) sits in `activating`, never `failed`. A DRIFT read already proves a default route
    # exists, so the hint must say the unit is not applying the qdisc and point at its journal.
    v = _prov(_block(qdisc=CAM7_UNPACED + "|", active="activating", restarts="4"))
    assert "default route exists" in v and "journalctl -u cambox-egress-pacing" in v, v
    assert "4 failed round" in v, v
    assert "no default route yet" not in v and "setup-device" not in v, v
    # setup-device REFUSES without tc; it installs nothing.
    v = _prov(_block(qdisc="__TC_ABSENT__"))
    assert "refuses to run without tc" in v and "setup-device.sh does" not in v, v


def test_boot_script_logs_a_missing_route_only_at_the_edges_of_a_round(tmp_path):
    # Review round 2: a route-less box (dead NIC, a bench) retries forever; logging the
    # "no default route" line on every attempt would put ~35 lines into the journal every ~90 s.
    p, _ = _boot_script(tmp_path)
    fk = _fakes(tmp_path)
    attempts = int(_lib('echo "$CAMBOX_EGRESS_PACING_RETRY_ATTEMPTS"').stdout)
    r = _run(f'"{p}"', env={**fk, "FAKE_IP_ROUTE": "", "CAMBOX_EGRESS_PACING_RETRY_SLEEP_S": "0"})
    assert r.returncode == 1
    assert (tmp_path / "ip.calls").read_text().strip() == str(attempts)
    assert r.stderr.count("no default route yet") == 2, r.stderr
    assert r.stderr.count("FAILED after") == 1, r.stderr


# =================================================================================================
# The generated on-box script + the checked-in unit
# =================================================================================================

def _boot_script(tmp_path):
    r = _lib("cambox_egress_pacing_boot_script")
    assert r.returncode == 0, r.stderr
    p = tmp_path / "cambox-egress-pacing"
    p.write_text(r.stdout)
    p.chmod(0o755)
    return p, r.stdout


def test_boot_script_is_valid_bash_and_embeds_the_one_apply_command(tmp_path):
    p, text = _boot_script(tmp_path)
    assert text.startswith("#!/bin/bash\n")
    assert subprocess.run(["bash", "-n", str(p)]).returncode == 0
    apply = _lib("cambox_egress_pacing_apply_cmd").stdout
    assert apply.strip() and apply.strip() in text, "the boot script runs the lib's own apply command"
    assert re.search(r"(?m)^set -euo pipefail$", text)


def test_boot_script_applies_once_and_exits_zero(tmp_path):
    p, _ = _boot_script(tmp_path)
    fk = _fakes(tmp_path)
    r = _run(f'"{p}"', env={**fk, "FAKE_IP_ROUTE": ROUTE_ENP2})
    assert r.returncode == 0, r.stderr
    assert len(_argv(tmp_path, "tc")) == 1


def test_boot_script_waits_for_a_late_default_route(tmp_path):
    p, _ = _boot_script(tmp_path)
    fk = _fakes(tmp_path)
    r = _run(f'"{p}"', env={**fk, "FAKE_IP_ROUTE": ROUTE_ENP2, "FAKE_IP_EMPTY_FIRST": "3",
                            "CAMBOX_EGRESS_PACING_RETRY_SLEEP_S": "0"})
    assert r.returncode == 0, r.stderr
    assert (tmp_path / "ip.calls").read_text().strip() == "4"
    assert _argv(tmp_path, "tc") == [
        "qdisc replace dev enp2s0 root fq maxrate 400mbit flow_limit 2000 limit 20000"]


def test_boot_script_gives_up_loud_after_the_bounded_retry(tmp_path):
    p, _ = _boot_script(tmp_path)
    fk = _fakes(tmp_path)
    attempts = int(_lib('echo "$CAMBOX_EGRESS_PACING_RETRY_ATTEMPTS"').stdout)
    sleep_s = int(_lib('echo "$CAMBOX_EGRESS_PACING_RETRY_SLEEP_S"').stdout)
    assert 50 <= attempts * sleep_s <= 70, "the design's ~60 s bound for a DHCP-late default route"
    r = _run(f'"{p}"', env={**fk, "FAKE_IP_ROUTE": "", "CAMBOX_EGRESS_PACING_RETRY_SLEEP_S": "0"})
    assert r.returncode == 1
    assert (tmp_path / "ip.calls").read_text().strip() == str(attempts)
    assert "FAILED" in r.stderr and "UNPACED" in r.stderr


def test_the_checked_in_unit_runs_the_generated_script_at_boot():
    text = _read(UNIT)
    lines = [ln.strip() for ln in text.splitlines()]
    for want in ("Type=oneshot", "RemainAfterExit=yes", "After=network-online.target",
                 "Wants=network-online.target", "WantedBy=multi-user.target"):
        assert want in lines, want
    path = _lib('echo "$CAMBOX_EGRESS_PACING_SCRIPT_PATH"').stdout.strip()
    assert f"ExecStart={path}" in lines
    timeout = [ln for ln in lines if ln.startswith("TimeoutStartSec=")]
    assert timeout and int(timeout[0].split("=", 1)[1]) > 60, "a bound above the retry budget"
    name = _lib('echo "$CAMBOX_EGRESS_PACING_SERVICE_NAME"').stdout.strip()
    assert UNIT.name == f"{name}.service"
    src = _lib('echo "$CAMBOX_EGRESS_PACING_UNIT_SRC"').stdout.strip()
    assert pathlib.Path(src).resolve() == UNIT.resolve()


def test_the_unit_keeps_retrying_after_a_late_link():
    # Review round 1: a box that boots before its switch port has carrier fails the whole ~60 s
    # retry; without a restart it would stay unpaced for its whole uptime.
    text = _read(UNIT)
    unit_section = text.split("[Service]")[0]
    service_section = text.split("[Service]")[1].split("[Install]")[0]
    assert re.search(r"(?m)^StartLimitIntervalSec=0$", unit_section), "never give up restarting"
    assert re.search(r"(?m)^Restart=on-failure$", service_section)
    rs = re.search(r"(?m)^RestartSec=(\d+)$", service_section)
    assert rs and int(rs.group(1)) > 0, "a pause between the retry rounds"


# =================================================================================================
# setup-device.sh: installs + enables, enable-only, in the rw window
# =================================================================================================

def _setup_step(text):
    start = text.find("\n# [egress-pacing]:")
    assert start >= 0, "setup-device.sh has the [egress-pacing] sub-step"
    header_end = text.find("\n# =====", start)       # the sub-step header's closing banner
    end = text.find("\n# =====", header_end + 1)     # the next section's opening banner
    return text[start:end]


def test_setup_device_sources_the_lib_and_installs_in_the_rw_window():
    text = _read(SETUP_DEVICE)
    assert re.search(r'(?m)^\. "\$HERE/lib/cambox-egress-pacing\.sh"', text)
    step = text.find("\n# [egress-pacing]:")
    assert text.find("STEP 17c:") < step < text.find("# STEP 18: Configure read-only"), \
        "after the DSCP oneshot, before STEP 18 flips root read-only"


def test_setup_device_step_is_enable_only():
    step = _setup_step(_read(SETUP_DEVICE))
    code = "\n".join(ln for ln in step.splitlines() if not ln.lstrip().startswith("#"))
    assert "cambox_egress_pacing_boot_script >" in code
    assert '"$CAMBOX_EGRESS_PACING_UNIT_SRC"' in code
    assert 'systemctl enable "$CAMBOX_EGRESS_PACING_SERVICE_NAME"' in code
    assert "is-enabled" in code and "fail " in code
    for banned in ("systemctl start", "systemctl restart", "tc qdisc", "cambox_egress_pacing_apply_cmd"):
        assert banned not in code, f"setup-device never applies live: {banned}"


def test_setup_device_step_writes_script_unit_and_enables(tmp_path):
    step = _setup_step(_read(SETUP_DEVICE))
    fk = _fakes(tmp_path)
    prelude = (
        'GREEN=; NC=; fail() { echo "FAIL $1"; exit 1; }\n'
        f'CAMBOX_EGRESS_PACING_SCRIPT_PATH="{tmp_path}/sbin/cambox-egress-pacing"\n'
        f'CAMBOX_EGRESS_PACING_SERVICE_PATH="{tmp_path}/systemd/cambox-egress-pacing.service"\n')
    r = _lib(prelude + step, env=fk)
    assert r.returncode == 0, r.stdout + r.stderr
    script = tmp_path / "sbin" / "cambox-egress-pacing"
    assert os.access(script, os.X_OK)
    assert script.read_text() == _lib("cambox_egress_pacing_boot_script").stdout
    assert (tmp_path / "systemd" / "cambox-egress-pacing.service").read_text() == _read(UNIT)
    assert _argv(tmp_path, "systemctl") == [
        "daemon-reload", "enable cambox-egress-pacing", "is-enabled cambox-egress-pacing"]
    r = _lib(prelude + step, env={**fk, "FAKE_ENABLED": "disabled"})
    assert r.returncode != 0 and "FAIL" in r.stdout


def test_setup_device_refuses_before_any_write_without_the_systemd_sibling():
    text = _read(SETUP_DEVICE)
    lines = [ln for ln in text.splitlines() if not ln.lstrip().startswith("#")]
    body = "\n".join(lines)
    check = body.find('[ -f "$CAMBOX_EGRESS_PACING_UNIT_SRC" ]')
    first_write = body.find("\nensure_root_writable\n")
    assert 0 <= check < first_write, "the pre-flight runs before the rw remount / first write"


# =================================================================================================
# verify-device.sh (ap): a hard gate, documented three times, before (q), outside executed slices
# =================================================================================================

def _ap_block(text):
    ap = text.find("\n# (ap) ")
    af = text.find("\n# (af) ", ap)
    assert 0 <= ap < af, "(ap) sits right before (af)"
    return text[ap:af]


def test_verify_device_documents_and_places_ap():
    text = _read(VERIFY_DEVICE)
    guard = text.find("never run the live SSH flow below.")
    header = text[:text.find("set -euo pipefail")]
    assert "(ap)" in header and "egress pacing" in header
    usage = text[text.find("usage() {"):]
    assert "(ap)" in usage[:usage.find("\nEOF\n")]
    ap = text.find("\n# (ap) ")
    assert guard < text.find("\n# (ae) ") < ap < text.find("\n# (af) ") < text.rfind("# (q) .bak cruft drift")
    # Never inside the slices other pytests EXECUTE: (ao)..(an) and (an)..(q).
    assert not (text.find("\n# (ao) ") < ap < text.rfind("# (q) .bak cruft drift"))
    assert re.search(r'(?m)^\. "\$HERE/lib/cambox-egress-pacing\.sh"', text)


def test_verify_device_ap_is_a_hard_gate():
    block = _ap_block(_read(VERIFY_DEVICE))
    assert "cambox_egress_pacing_gather_remote_snippet" in block
    assert "cambox_egress_pacing_provision_verdict" in block
    assert "fail " in block and 'warn "' not in block


def _run_ap(tmp_path, block_out, ssh_rc=0):
    prelude = ('ok() { echo "OK $1"; }\nfail() { echo "FAIL $1"; }\n'
               f'ssh_box() {{ printf "%s" "$AP_BLOCK"; return {ssh_rc}; }}\n')
    return _lib(prelude + _ap_block(_read(VERIFY_DEVICE)), env={"AP_BLOCK": block_out})


def test_verify_device_ap_runs(tmp_path):
    r = _run_ap(tmp_path, _block())
    assert r.returncode == 0 and r.stdout.startswith("OK ") and "enp3s0" in r.stdout, r.stdout + r.stderr
    r = _run_ap(tmp_path, _block(qdisc=CAM7_UNPACED + "|"))
    assert r.stdout.startswith("FAIL ") and "pfifo_fast" in r.stdout, r.stdout
    r = _run_ap(tmp_path, _block(enabled="disabled"))
    assert r.stdout.startswith("FAIL ") and "not enabled" in r.stdout, r.stdout
    r = _run_ap(tmp_path, "", ssh_rc=255)
    assert r.stdout.startswith("FAIL ") and "rc=255" in r.stdout, r.stdout


def test_verify_device_ap_names_the_fix_for_a_box_that_only_needs_a_start(tmp_path):
    # Review round 1: enabled + installed, never run since install, runtime qdisc gone. Re-running
    # setup-device (apt, GRUB, an rw remount) would change nothing: `systemctl start` is the fix.
    r = _run_ap(tmp_path, _block(qdisc=CAM7_UNPACED + "|", active="inactive"))
    assert r.stdout.startswith("FAIL "), r.stdout
    assert "systemctl start cambox-egress-pacing" in r.stdout, r.stdout
    assert "setup-device" not in r.stdout, r.stdout
    r = _run_ap(tmp_path, _block(script_sha="0" * 64))
    assert r.stdout.startswith("FAIL ") and "stale" in r.stdout and "setup-device" in r.stdout, r.stdout


# =================================================================================================
# The E2E [0/8] row: report-only, one line per vetted cambox, never aborts
# =================================================================================================

def _e2e(tmp_path, targets, boxes, pre=""):
    fk = _fakes(tmp_path)
    for ip, block in boxes.items():
        (tmp_path / "ssh" / f"{ip}.txt").write_text(block)
    return _lib(f'{pre}\ncambox_egress_pacing_e2e_report "$TARGETS" pw\necho "AFTER rc=$?"',
                env={**fk, "TARGETS": targets})


def test_e2e_report_reads_every_target_and_never_aborts(tmp_path):
    r = _e2e(tmp_path, "cam1=10.0.0.61 cam3=10.0.0.63 cam7=10.0.0.67",
             {"10.0.0.61": _block(), "10.0.0.63": _block(qdisc=CAM7_UNPACED + "|")})
    assert "AFTER rc=0" in r.stdout, r.stdout + r.stderr
    out = r.stdout
    assert "[0/8]" in out and "report-only" in out
    assert re.search(r"(?m)^    ok: cam1 .*fq", out), out
    assert re.search(r"(?m)^    WARNING: cam3 .*pfifo_fast", out), out
    assert re.search(r"(?m)^    WARNING: cam7 .*UNKNOWN", out), out
    assert "1 ok, 2 warning" in out
    # Review round 1: the row names the matching fix (cam3's unit ran, the qdisc changed after),
    # never a blanket "not permanent".
    assert re.search(r"(?m)^    WARNING: cam3 .*systemctl restart cambox-egress-pacing", out), out
    assert "not permanent" not in out, out


def test_e2e_report_skips_an_acked_box(tmp_path):
    pre = 'cambox_offline_ack_is_acked() { [ "$1" = cam5 ]; }'
    r = _e2e(tmp_path, "cam1=10.0.0.61 cam5=10.0.0.65", {"10.0.0.61": _block()}, pre=pre)
    assert "AFTER rc=0" in r.stdout
    assert re.search(r"(?m)^    skip: cam5 ", r.stdout), r.stdout
    assert not any("10.0.0.65" in a for a in _argv(tmp_path, "sshpass"))


def test_e2e_report_with_no_targets_is_one_note(tmp_path):
    r = _e2e(tmp_path, "", {})
    assert "AFTER rc=0" in r.stdout and "no cambox" in r.stdout


def test_recording_e2e_calls_the_row_once_after_leg_health_before_the_optical_preflight():
    text = _read(E2E)
    assert re.search(r'(?m)^\. "\$HERE/lib/cambox-egress-pacing\.sh"', text)
    call = 'cambox_egress_pacing_e2e_report "$LEG_HEALTH_TARGETS" "$CAM_PW"'
    assert text.count(call) == 1
    at = text.find(call)
    assert text.find("for _lht in $LEG_HEALTH_TARGETS; do") < at
    assert at < text.find('echo "[0/8] optical head-end blur/shutter preflight')
    line = text[text.rfind("\n", 0, at) + 1:text.find("\n", at)]
    assert line.strip() == call, "a plain statement: its report-only output, never an exit"
