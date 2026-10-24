#!/usr/bin/env python3
"""strih-display-watch (issue 1424): keep the kiosk X screen usable with and without monitors.

WHY (owner, 24.10.2026: "to musi vediet fungovat aj bez monitora aj po zapojeni monitora aj po
zapnuti!!!"; main design 6386467303, layer 2): strih-pp booted with the venue dark. X found only the
Multiview monitor (a fixed vk-direct HDMI output OBS takes away from X), the autostart turned it off and
left an 8x8 screen, tint2 segfaulted in a restart loop on that empty screen, and when the operator's
monitor came on nothing put it into the X layout -- the openbox kiosk has no desktop daemon for that.

WHAT: a supervised session app (strih-display-watch.service, Restart=always) that reads
`xrandr --current` every 2 s (the cached state: no output re-probe, no DDC read on the monitors) and runs
the pure `decide`:
  * every connected OPERATOR output -- any output that is not a fixed HDMI output of the drm-output
    config (~/.camera-box/drm-output.json, read with the C module's grammar, drm_output_config.py) -- that
    has no mode gets `--auto`, right of the operator outputs already on; the first operator output is the
    primary;
  * a FIXED output X put into its layout (an X start with that monitor alone, an `xrandr --auto` by hand)
    goes `--off`: an X CRTC on it makes OBS's vk-direct acquire fail. A fixed output OBS holds shows no
    mode (or reads disconnected) and is never named;
  * with no operator output on, the screen is kept at least 1920x1080 (`--fb`, in the same call as any
    `--off`, which would otherwise shrink it to 8x8) and the primary flag is cleared;
  * an operator output that is on is never touched, unplugged or not (the X driver keeps its mode and
    lights it again when it comes back);
  * a config it cannot classify leaves every output alone (only the screen size is kept).
Every change is ONE xrandr call. A failed one is retried after 10 s and logged once per distinct error.

`--exec-when-screen CMD...` is the kiosk panel's guard (obs-box-panel.service): it waits until X has a
screen with a monitor on it, then execs CMD (tint2) in place -- tint2 never starts on an empty screen.
`--check-state FILE` is verify-strih's pass row. Std-only.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import Optional

POLL_S = 2.0
XRANDR_TIMEOUT_S = 5.0
RETRY_FAILED_S = 10.0
# the smallest screen kept with no operator output on: the kiosk's own panel pin (1920x1080)
MIN_FB = (1920, 1080)
# what the panel's guard calls "a screen" (an 8x8 screen is X with nothing on it)
MIN_SCREEN = (320, 240)
STATE_VERSION = 1
STATE_MAX_AGE_S = 15.0

_SCREEN_RE = re.compile(r"^Screen \d+: .*\bcurrent (\d+) x (\d+)")
_OUT_RE = re.compile(r"^(\S+) (connected|disconnected)( primary)?(?: (\d+)x(\d+)\+(-?\d+)\+(-?\d+))?(?:\s|$)")


@dataclass(frozen=True)
class Output:
    name: str
    connected: bool
    primary: bool
    w: Optional[int] = None
    h: Optional[int] = None
    x: Optional[int] = None
    y: Optional[int] = None

    @property
    def active(self):
        return self.w is not None


def parse_xrandr(text):
    """`xrandr --current` -> (screen (w, h) or None, [Output]) in the order X lists them. Mode lines and
    anything else are skipped; a text without a Screen line is (None, [])."""
    screen = None
    outs = []
    for line in (text or "").splitlines():
        m = _SCREEN_RE.match(line)
        if m:
            screen = (int(m.group(1)), int(m.group(2)))
            continue
        m = _OUT_RE.match(line)
        if m:
            geo = [int(g) for g in m.group(4, 5, 6, 7)] if m.group(4) else [None] * 4
            outs.append(Output(m.group(1), m.group(2) == "connected", bool(m.group(3)), *geo))
    if screen is None:
        return None, []
    return screen, outs


def fixed_connectors(config_text, module=None):
    """The fixed HDMI outputs of the drm-output config: [] when there is no config or it is disabled,
    the armed connectors when it is valid (any backend: both take the connector away from X), None when
    it cannot be classified (the watch then touches no output; also an unreadable file, `config_text` None).
    `module` = drm_output_config (a seam)."""
    if config_text is None:
        return None
    if not config_text.strip():
        return []
    if module is None:
        try:
            import drm_output_config as module  # noqa: PLC0415 -- installed beside this program
        except ImportError:
            return None
    try:
        verdict, outputs = module.drm_output_instances(config_text)
    except Exception:  # noqa: BLE001 -- an unreadable config is "cannot classify", never a crash
        return None
    if verdict == "ok":
        return [o["connector"] for o in outputs]
    if verdict == "disabled":
        return []
    return None


def hdmi_off_from_env(env=None):
    """The role's HDMI policy from the unit's EnvironmentFile (setup-strih step 16d writes it): `off` on
    the strih role, where no HDMI output is ever the desktop (issue 1346), else HDMI belongs to X."""
    return (os.environ if env is None else env).get("STRIH_DISPLAY_WATCH_HDMI", "") == "off"


def effective_fixed(fixed, outputs, hdmi_off):
    """The outputs kept out of X: the config's fixed ones, plus every HDMI output when the role's policy
    says HDMI is never the desktop. None (cannot classify) stays None: no output is touched then."""
    if fixed is None:
        return None
    out = list(fixed)
    if hdmi_off:
        out += [o.name for o in outputs if o.name.startswith("HDMI") and o.name not in out]
    return out


def screen_ready(screen, outputs, min_screen=MIN_SCREEN):
    """X has a screen with a monitor on it (the panel's guard)."""
    return (screen is not None and screen[0] >= min_screen[0] and screen[1] >= min_screen[1]
            and any(o.active for o in outputs))


def decide(screen, outputs, fixed, min_fb=MIN_FB):
    """The one xrandr call that brings X to the wanted layout (its arguments, [] = nothing to do).
    `fixed` = the fixed outputs ([] none, None = the config could not be classified)."""
    known = fixed is not None
    fixed = set(fixed or [])
    ops = [o for o in outputs if not known or o.name not in fixed]
    fixed_on = [o for o in outputs if known and o.name in fixed and o.active]
    ops_on = [o for o in ops if o.active] if known else [o for o in outputs if o.active]
    pending = [o for o in ops if o.connected and not o.active] if known else []
    head, tail = [], []
    for o in fixed_on:
        tail += ["--output", o.name, "--off"]
    primary_ok = any(o.primary for o in ops_on)
    target = None
    if not primary_ok and (ops_on or pending):
        target = sorted(ops_on, key=lambda o: (o.x, o.y))[0] if ops_on else pending[0]
    rightmost = max(ops_on, key=lambda o: o.x + o.w).name if ops_on else None
    if target is not None and target.active and known:
        tail += ["--output", target.name, "--primary"]
    for o in pending:
        tail += ["--output", o.name, "--auto"]
        if rightmost:
            tail += ["--right-of", rightmost]
        if o is target:
            tail.append("--primary")
        rightmost = o.name
    if not ops_on and not pending and screen is not None:
        small = screen[0] < min_fb[0] or screen[1] < min_fb[1]
        if small or fixed_on:
            head += ["--fb", "%dx%d" % (max(screen[0], min_fb[0]), max(screen[1], min_fb[1]))]
        if known and any(o.primary for o in outputs):
            head.append("--noprimary")
    return head + tail


def run_cmd(argv, timeout):
    """(rc, stdout, stderr) of argv; rc 124 on a timeout, 127 when the program is missing."""
    try:
        p = subprocess.run(argv, capture_output=True, text=True, timeout=timeout,
                           env=dict(os.environ, LC_ALL="C"))
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 124, "", "timed out after %.0f s" % timeout
    except OSError as e:
        return 127, "", str(e)


def default_config_text():
    """The drm-output config's text: "" when there is no file, None when it exists but cannot be read
    (EACCES, EIO, a directory ...: cannot classify -- no output is touched, never read as "no config")."""
    try:
        with open(os.path.expanduser("~/.camera-box/drm-output.json"), encoding="utf-8-sig") as fh:
            return fh.read()
    except FileNotFoundError:
        return ""
    except (OSError, UnicodeDecodeError):
        return None


def _outputs_summary(outputs, fixed):
    rows = []
    for o in outputs:
        if o.connected or o.active:
            rows.append([o.name, "connected" if o.connected else "disconnected", "on" if o.active else "off",
                         "primary" if o.primary else "", "fixed" if fixed and o.name in fixed else ""])
    return rows


class Watch:
    """One pass every POLL_S: read X, decide, apply, keep the state file."""

    def __init__(self, state_file, run=run_cmd, log=print, config_text=default_config_text, hdmi_off=False):
        self.state_file = state_file
        self.hdmi_off = hdmi_off
        self.run = run
        self.log = log
        self.config_text = config_text
        self.failed = {}          # tuple(args) -> the time it last failed
        self.logged_error = None  # (args, error) logged last
        self.x_down_logged = False
        self.last_apply = None
        self.last_error = None
        self.pending = []
        self.last_write = 0.0
        self.last_summary = None

    def pass_once(self, now):
        rc, out, err = self.run(["xrandr", "--current"], XRANDR_TIMEOUT_S)
        screen, outputs = parse_xrandr(out if rc == 0 else "")
        if screen is None:
            if not self.x_down_logged:
                self.log("X on %s not answering (xrandr rc=%d: %s) -- retrying every %.0f s"
                         % (os.environ.get("DISPLAY", "?"), rc, (err or "").strip()[:200], POLL_S))
                self.x_down_logged = True
            self._write(now, None, [], None)
            return
        if self.x_down_logged:
            self.log("X on %s answers again" % os.environ.get("DISPLAY", "?"))
            self.x_down_logged = False
        fixed = effective_fixed(fixed_connectors(self.config_text()), outputs, self.hdmi_off)
        summary = (screen, tuple(map(tuple, _outputs_summary(outputs, fixed))), tuple(fixed or ()), fixed is None)
        if summary != self.last_summary:
            self.log("watching %s: screen %dx%d; operator outputs %s; fixed outputs %s"
                     % (os.environ.get("DISPLAY", "?"), screen[0], screen[1],
                        ", ".join("%s (%s, %s%s)" % (r[0], r[1], r[2], ", primary" if r[3] else "")
                                  for r in _outputs_summary(outputs, fixed) if not r[4]) or "none connected",
                        ("unknown (the drm-output config cannot be classified -- no output is touched)"
                         if fixed is None else " ".join(fixed) or "none"))
                     + ("; HDMI never on the desktop (this role, issue 1346)" if self.hdmi_off else ""))
            self.last_summary = summary
        args = decide(screen, outputs, fixed)
        self.pending = args
        key = tuple(args)
        if args and now - self.failed.get(key, -1e18) >= RETRY_FAILED_S:
            arc, _o, aerr = self.run(["xrandr"] + args, XRANDR_TIMEOUT_S)
            if arc == 0:
                self.log("applied: xrandr %s" % " ".join(args))
                self.last_apply = {"epoch_s": time.time(), "args": args}
                self.last_error = None
                self.failed.pop(key, None)
                self.logged_error = None
                self.pending = []
            else:
                msg = (aerr or "").strip().splitlines()[-1:] or ["rc=%d" % arc]
                self.last_error = "xrandr %s failed (rc=%d): %s" % (" ".join(args), arc, msg[0][:200])
                self.failed[key] = now
                if self.logged_error != (key, msg[0]):
                    self.log("%s -- retrying every %.0f s" % (self.last_error, RETRY_FAILED_S))
                    self.logged_error = (key, msg[0])
        elif not args:
            self.last_error = None
        self._write(now, screen, outputs, fixed)

    def _write(self, now, screen, outputs, fixed):
        self.last_write = now
        state = {"version": STATE_VERSION, "updated_epoch_s": time.time(),
                 "screen": list(screen) if screen else None,
                 "outputs": _outputs_summary(outputs, fixed), "fixed": fixed,
                 "last_apply": self.last_apply, "last_error": self.last_error, "pending": self.pending}
        tmp = "%s.tmp.%d" % (self.state_file, os.getpid())
        try:
            with open(tmp, "w", encoding="utf-8") as fh:
                json.dump(state, fh)
            os.replace(tmp, self.state_file)
        except OSError as e:
            self.log("cannot write the state file %s: %s" % (self.state_file, e))


def state_verdict(state, now, max_age=STATE_MAX_AGE_S):
    """(ok, one line) for verify-strih's pass row."""
    if not isinstance(state, dict) or state.get("version") != STATE_VERSION:
        return False, "no usable state (version %r)" % (state.get("version") if isinstance(state, dict) else None)
    age = now - float(state.get("updated_epoch_s") or 0)
    if age > max_age:
        return False, "the last pass was %.0f s ago (more than %.0f s): the watch does not run" % (age, max_age)
    screen = state.get("screen")
    if not screen:
        return False, "X on :0 does not answer the watch (last pass %.0f s ago)" % age
    outs = state.get("outputs") or []
    ops = ["%s (%s)" % (r[0], ", ".join(x for x in r[1:4] if x)) for r in outs if not (len(r) > 4 and r[4])]
    fixed = state.get("fixed")
    desc = "screen %dx%d; operator outputs: %s; fixed outputs kept out of X: %s" % (
        screen[0], screen[1], "; ".join(ops) or "none connected",
        "unknown (config cannot be classified)" if fixed is None else " ".join(fixed) or "none")
    if screen[0] < MIN_SCREEN[0] or screen[1] < MIN_SCREEN[1]:
        return False, "the screen is %dx%d -- no usable screen (%s)" % (screen[0], screen[1], desc)
    if state.get("last_error") and state.get("pending"):
        return False, "%s -- the layout is not applied (%s)" % (state["last_error"], desc)
    return True, "last pass %.0f s ago: %s" % (age, desc)


def check_state_file(path, now=None):
    try:
        with open(path, encoding="utf-8") as fh:
            state = json.load(fh)
    except (OSError, ValueError) as e:
        return False, "cannot read %s: %s" % (path, e)
    return state_verdict(state, time.time() if now is None else now)


def exec_when_screen(argv, run=run_cmd, sleep=time.sleep, execv=os.execv, log=print):
    """Wait until X has a screen with a monitor on it, then exec argv in place."""
    waited = False
    while True:
        rc, out, _err = run(["xrandr", "--current"], XRANDR_TIMEOUT_S)
        screen, outputs = parse_xrandr(out if rc == 0 else "")
        if screen_ready(screen, outputs):
            if waited:
                log("a monitor is on (screen %dx%d) -- starting %s" % (screen[0], screen[1], argv[0]))
            execv(argv[0], argv)
            return
        if not waited:
            log("waiting for a monitor on %s before starting %s (screen %s) -- checking every %.0f s"
                % (os.environ.get("DISPLAY", "?"), argv[0],
                   "%dx%d" % screen if screen else "not answering", POLL_S))
            waited = True
        sleep(POLL_S)


def default_state_file():
    return os.path.join(os.environ.get("XDG_RUNTIME_DIR") or "/run/user/%d" % os.getuid(),
                        "strih-display-watch.json")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--state-file", default=None)
    ap.add_argument("--check-state", metavar="FILE")
    ap.add_argument("--exec-when-screen", nargs=argparse.REMAINDER, metavar="CMD")
    ap.add_argument("--once", action="store_true", help="one pass, then exit")
    a = ap.parse_args(argv)

    def log(msg):
        print(msg, flush=True)

    if a.check_state:
        ok, line = check_state_file(a.check_state)
        print(line)
        return 0 if ok else 1
    if a.exec_when_screen is not None:
        if not a.exec_when_screen:
            ap.error("--exec-when-screen needs a command")
        exec_when_screen(a.exec_when_screen, log=log)
        return 1  # reached only when the exec itself failed (os.execv raises then)
    w = Watch(a.state_file or default_state_file(), log=log, hdmi_off=hdmi_off_from_env())
    while True:
        w.pass_once(time.monotonic())
        if a.once:
            return 0
        time.sleep(POLL_S)


if __name__ == "__main__":
    sys.exit(main())
