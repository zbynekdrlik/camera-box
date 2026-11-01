"""Issue 1387: the dev1 5-minute watchdogs must not hit the rig at one instant.

About 18 units fired at the same moment, every 5 minutes, and every such batch made the strih JPEG XS
program miss frames (dropped_busy 2-3, encode max 75-95 ms, 20:46:53 and 20:51:56 on 1.11.2026 on an
idle rig) and put a copies/gaps burst into E2E 41338338452. "No watchdog may cause frame loss".
"""
import configparser
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[2]


def seconds(value):
    m = re.fullmatch(r"(\d+)\s*(s|min|h)?", value.strip())
    assert m, value
    return int(m.group(1)) * {"s": 1, "min": 60, "h": 3600, None: 1}[m.group(2)]


def timer(path):
    c = configparser.ConfigParser(strict=False, interpolation=None)
    c.optionxform = str
    c.read(path)
    return c["Timer"]


def five_minute_timers():
    out = [p for p in sorted((ROOT / "systemd").glob("*.timer"))
           if seconds(timer(p).get("OnUnitActiveSec", "0")) == 300]
    assert len(out) >= 20, "the batch this guards"
    return out


def test_every_five_minute_timer_spreads_its_start_over_two_minutes():
    for p in five_minute_timers():
        delay = timer(p).get("RandomizedDelaySec")
        assert delay is not None, f"{p.name} fires with the batch"
        assert seconds(delay) == 120, p.name


def test_the_bundle_gather_never_competes_with_obs():
    unit = (ROOT / "systemd/strih-bundle-state-server.service").read_text()
    assert re.search(r"^Nice=19$", unit, re.M), "one gather holds a core for seconds"
    assert re.search(r"^CPUAffinity=0 1$", unit, re.M), "OBS runs on cores 2-15"
