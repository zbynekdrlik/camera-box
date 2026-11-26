"""issue 1545 -- a confirmed clock loss DURING A PRODUCTION reaches the owner through the airuleset
`page:` class.

WHY: on 19.11.2026 cam3 ran the whole SNV evening service on NTP only. The dev1 clock watchdog read
`verdict=NO_CLOCK reason=clock_alarm` every pass from 17:27 and fired its alert every pass, but its
`dante-clock-<box>-<bucket>` key is not one the owner policy lets through (airuleset
OWNER_PING_ALLOW: only acctblock / credential-dead / healthz / `page:<check>:<n>`), so nothing
reached a human. The stream-output-stuck watchdog (issue 1460) is the precedent: a
`page:<check>:<bucket>` key, which airuleset caps at one message per check per 10 minutes.

What these tests pin (scripts/dantesync-clock-alert-watchdog.sh):
- NO_CLOCK, NO_DANTESYNC and the fleet-wide DNS_UNRESOLVABLE, confirmed, in a venue that is in a
  production (`yes`, or `unknown`: a broken probe never silences a broadcast) -> ONE page per venue
  per pass, `page:dante-clock-<venue>:<bucket>`, listing every box without the clock.
- no production -> no page (the plain bucketed notify still runs, as before).
- MGMT_DEAD and GM_CHANGED are not clock losses -> never a page.
- the production read (DANTE_CLOCK_PRODUCTION_CMD seam, `<cmd> <venue>`) runs only when a page is
  due, once per venue, cached DANTE_CLOCK_PRODUCTION_TTL_S across passes.

Driven end-to-end through the real watchdog in subprocesses with the DANTE_CLOCK_* seams and a full
DANTE_CLOCK_NODES roster on TEST-NET addresses (no live rig); run it under `unshare -n` too.
"""
import os
import pathlib
import re
import stat
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_WATCHDOG = _ROOT / "scripts" / "dantesync-clock-alert-watchdog.sh"

_NOW = 1789379394
_INTERVAL = 600
GM = "10.77.9.230"
CAM3 = "192.0.2.13"
CAM5 = "192.0.2.15"
PP = "192.0.2.40"

_PAGE_RE = re.compile(r"page:[a-z0-9][a-z0-9.-]{0,63}(:\S*)?")


def _status(is_locked="true", mode="LOCK"):
    return (
        '{"offset_ns":1,"settled":true,"updated_ts":%d,"is_locked":%s,"mode":"%s",'
        '"gm_source_ip":"%s","ntp_step_storm":false,"ntp_steps_last_hour":null}'
        % (_NOW, is_locked, mode, GM)
    )


LOST = _status(is_locked="false", mode="ACQ")
OK = _status()


def _exe(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return path


def _run(tmp, statuses, production="yes", nodes=None, passes=1, now_step=60, dry=True,
         box_up="1", mgmt="1", extra_env=None):
    """Run `passes` watchdog passes. `statuses` maps ip -> status JSON (absent = :8898 dead).
    `production` is the answer of the production seam: one word for every venue, or a dict.
    Returns (list of per-pass stderr, list of production seam calls)."""
    tmp = pathlib.Path(tmp)
    tmp.mkdir(parents=True, exist_ok=True)
    cases = "".join(
        f"  {ip}) cat <<'JSON'\n{body}\nJSON\n  exit 0 ;;\n" for ip, body in statuses.items()
    )
    fetch = _exe(tmp / "fetch.sh", f"#!/usr/bin/env bash\ncase \"$1\" in\n{cases}esac\nexit 1\n")
    up = _exe(tmp / "up.sh", f"#!/usr/bin/env bash\nprintf '{box_up}'\n")
    mg = _exe(tmp / "mgmt.sh", f"#!/usr/bin/env bash\nprintf '{mgmt}'\n")
    calls = tmp / "production-calls.txt"
    if isinstance(production, dict):
        pcases = "".join(f"  {v}) echo {a} ;;\n" for v, a in production.items())
        pbody = f"case \"$1\" in\n{pcases}  *) echo unknown ;;\nesac\n"
    else:
        pbody = f"echo {production}\n"
    prod = _exe(tmp / "prod.sh", f"#!/usr/bin/env bash\necho \"$1\" >> '{calls}'\n{pbody}")
    roster = nodes if nodes is not None else f"cam3|{CAM3}|fixed|video"
    env = dict(os.environ)
    for k in list(env):
        if k.startswith("DANTE_CLOCK_") or k in ("AVSYNC_PRODUCTION", "RIG_GRANDMASTER_HOST"):
            del env[k]
    env.update({
        "DANTE_CLOCK_NODES": roster,
        "DANTE_CLOCK_LOCAL_NODES": "",
        "DANTE_CLOCK_REACH_NODES": "",
        "DANTE_CLOCK_FETCH_CMD": str(fetch),
        "DANTE_CLOCK_BOX_UP_CMD": str(up),
        "DANTE_CLOCK_MGMT_SSH_CMD": str(mg),
        "DANTE_CLOCK_PRODUCTION_CMD": str(prod),
        "RIG_GRANDMASTER_IP": GM,
        "DANTE_CLOCK_CONFIRM_THRESHOLD": "1",
        "DANTE_CLOCK_REPING_INTERVAL_S": str(_INTERVAL),
        "DANTE_CLOCK_VERSION_PIN": "1.35.17",
        "DANTE_CLOCK_ALERT_STATE_DIR": str(tmp),
        "AIRULESET_NOTIFY": str(tmp / "no-notify.py"),
    })
    env.update(extra_env or {})
    outs = []
    for i in range(passes):
        env["DANTE_CLOCK_NOW"] = str(_NOW + now_step * i)
        args = ["bash", str(_WATCHDOG)] + (["--dry-run"] if dry else [])
        r = subprocess.run(args, capture_output=True, text=True, env=env, timeout=120)
        assert r.returncode == 0, f"rc={r.returncode}\n{r.stderr}"
        outs.append(r.stderr)
    seen = calls.read_text().split() if calls.exists() else []
    return outs, seen


def _page_lines(err):
    return [ln for ln in err.splitlines() if "WOULD page" in ln]


def test_a_confirmed_no_clock_in_a_production_pages_the_venue(tmp_path):
    (err,), calls = _run(tmp_path, {CAM3: LOST}, production="yes")
    pages = _page_lines(err)
    assert len(pages) == 1, err
    assert "page:dante-clock-snv" in pages[0], pages
    assert "cam3" in pages[0], pages
    # the plain bucketed notify is unchanged next to the page
    assert "WOULD alert (dedup-key=dante-clock-cam3-" in err, err
    assert calls == ["snv"], calls


def test_no_production_means_no_page_but_the_plain_alert_stays(tmp_path):
    (err,), calls = _run(tmp_path, {CAM3: LOST}, production="no")
    assert not _page_lines(err), err
    assert "WOULD alert (dedup-key=dante-clock-cam3-" in err, err
    assert "page withheld" in err and "snv" in err, err
    assert calls == ["snv"], calls


def test_an_unknown_production_state_still_pages(tmp_path):
    (err,), _ = _run(tmp_path, {CAM3: LOST}, production="unknown")
    assert len(_page_lines(err)) == 1, err


def test_a_garbage_production_answer_counts_as_unknown_and_pages(tmp_path):
    (err,), _ = _run(tmp_path, {CAM3: LOST}, production="maybe")
    assert len(_page_lines(err)) == 1, err


def test_a_fleet_wide_loss_is_one_page_per_venue_listing_every_box(tmp_path):
    nodes = f"cam3|{CAM3}|fixed|video cam5|{CAM5}|fixed|video stream-pp|{PP}|fixed|video-pp"
    (err,), calls = _run(tmp_path, {CAM3: LOST, CAM5: LOST, PP: LOST}, nodes=nodes,
                         production={"snv": "yes", "pp": "no"})
    pages = _page_lines(err)
    assert len(pages) == 1, err
    assert "page:dante-clock-snv" in pages[0] and "cam3" in pages[0] and "cam5" in pages[0], pages
    assert "stream-pp" not in pages[0], pages
    assert sorted(calls) == ["pp", "snv"], calls


def test_a_pp_node_pages_its_own_venue(tmp_path):
    nodes = f"stream-pp|{PP}|fixed|video-pp"
    (err,), calls = _run(tmp_path, {PP: LOST}, nodes=nodes, production={"pp": "yes"})
    pages = _page_lines(err)
    assert len(pages) == 1 and "page:dante-clock-pp" in pages[0] and "stream-pp" in pages[0], err
    assert calls == ["pp"], calls


def test_a_dead_dantesync_on_a_live_box_pages(tmp_path):
    (err,), _ = _run(tmp_path, {}, production="yes", box_up="1")
    pages = _page_lines(err)
    assert len(pages) == 1 and "cam3" in pages[0], err
    assert "WOULD alert (dedup-key=dante-clock-nohttp-cam3-" in err, err


def test_a_down_box_is_skip_and_never_a_page(tmp_path):
    (err,), calls = _run(tmp_path, {}, production="yes", box_up="0")
    assert not _page_lines(err), err
    assert calls == [], calls


def test_mgmt_dead_is_not_a_clock_loss_and_never_pages(tmp_path):
    (err,), calls = _run(tmp_path, {CAM3: OK}, production="yes", mgmt="0")
    assert "WOULD alert (dedup-key=dante-clock-cam3-mgmt-" in err, err
    assert not _page_lines(err), err
    assert calls == [], calls


def test_a_healthy_fleet_reads_no_production_and_pages_nothing(tmp_path):
    (err,), calls = _run(tmp_path, {CAM3: OK}, production="yes")
    assert not _page_lines(err), err
    assert calls == [], calls


def test_an_unconfirmed_fault_does_not_page(tmp_path):
    (err,), calls = _run(tmp_path, {CAM3: LOST}, production="yes",
                         extra_env={"DANTE_CLOCK_CONFIRM_THRESHOLD": "2"})
    assert not _page_lines(err), err
    assert calls == [], calls


def test_an_unresolvable_grandmaster_dns_pages_the_home_venue(tmp_path):
    resolver = _exe(tmp_path / "getent.sh", "#!/usr/bin/env bash\nexit 2\n")
    (err,), _ = _run(tmp_path, {CAM3: OK}, production="yes",
                     extra_env={"RIG_GRANDMASTER_IP": "", "RIG_GRANDMASTER_GETENT": str(resolver)})
    pages = _page_lines(err)
    assert len(pages) == 1 and "page:dante-clock-snv" in pages[0], err
    assert "video-clock.lan" in pages[0], pages


def test_the_production_read_is_cached_across_passes_within_the_ttl(tmp_path):
    _, calls = _run(tmp_path, {CAM3: LOST}, production="yes", passes=3, now_step=60)
    assert calls == ["snv"], calls


def test_the_production_read_is_repeated_after_the_ttl(tmp_path):
    _, calls = _run(tmp_path, {CAM3: LOST}, production="yes", passes=2, now_step=301)
    assert calls == ["snv", "snv"], calls


def test_a_persisting_fault_pages_every_pass_with_the_bucketed_page_key(tmp_path):
    outs, _ = _run(tmp_path, {CAM3: LOST}, production="yes", passes=2, now_step=60)
    for err in outs:
        assert len(_page_lines(err)) == 1, err


def test_the_real_notify_call_carries_a_page_key_airuleset_accepts(tmp_path):
    log = tmp_path / "notify-argv.txt"
    notify = _exe(
        tmp_path / "notify.py",
        "#!/usr/bin/env python3\nimport sys\n"
        f"open({str(log)!r}, 'a').write('\\x1f'.join(sys.argv[1:]) + '\\n')\n",
    )
    _run(tmp_path, {CAM3: LOST}, production="yes", dry=False,
         extra_env={"AIRULESET_NOTIFY": str(notify)})
    calls = [c.split("\x1f") for c in log.read_text().splitlines()]
    keys = [c[c.index("--dedup-key") + 1] for c in calls]
    page_keys = [k for k in keys if k.startswith("page:")]
    assert len(page_keys) == 1, keys
    assert _PAGE_RE.fullmatch(page_keys[0]), page_keys
    assert page_keys[0].startswith("page:dante-clock-snv:"), page_keys
    body = next(c for c in calls if c[c.index("--dedup-key") + 1] == page_keys[0])
    assert "cam3" in body[body.index("--body") + 1], body
    # the plain bucketed card is still sent beside the page
    assert any(k.startswith("dante-clock-cam3-") for k in keys), keys


def test_the_page_seam_is_production_gated_in_the_rule_doc():
    rule = (_ROOT / ".claude" / "rules" / "dantesync-clock-alert-watchdog.md").read_text()
    assert "page:dante-clock-<venue>" in rule, "document the page class in the watchdog rule"
