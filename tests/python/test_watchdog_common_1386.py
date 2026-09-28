"""issue 1386 item 3 -- the ONE shared glue of the dev1 alert watchdogs.

scripts/lib/watchdog-common.sh holds every helper that was the same code (bash `declare -f`) in at
least three watchdogs: read_state_field, write_state_field, clear_throttle, clear_box_throttle,
clear_source_throttle, source_key, netreach_box_alerted, fetch_bundle_json. A helper whose code
differed stayed local; where such a local copy shares a lib name it is defined AFTER the source
line, so the local copy is the one that runs. This file pins:

  * the lib's shape: functions only, no shell option changed, the exact name set;
  * its behaviour under the callers' strictness (`set -uo pipefail`);
  * no watchdog anywhere (sourcing the lib or not) carries the lib's code again (compared by
    `declare -f`, so a reformatted copy is caught too);
  * the override list the lib header names -- a watchdog that sources the lib may keep a local copy
    of a lib name ONLY if it is listed here, defined after the source line;
  * no dangling call -- every watchdog that calls a lib helper either sources the lib or defines it.

Tier-0: pure python + bash subprocesses (no cargo). The byte-identity of the move itself was proven
per watchdog with `declare -f` (old vs new) and a network-less --dry-run replay (issue 1386 evidence).
"""
import pathlib
import re
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_LIB = _SCRIPTS / "lib" / "watchdog-common.sh"
_SOURCE_LINE = '. "$HERE/lib/watchdog-common.sh"'

LIB_FUNCS = {
    "read_state_field", "write_state_field", "clear_throttle", "clear_box_throttle",
    "clear_source_throttle", "source_key", "netreach_box_alerted", "fetch_bundle_json",
}

# The local copies the lib header documents: name -> watchdogs that keep their own (different) code.
ALLOWED_OVERRIDES = {
    "write_state_field": {
        # the older copy that writes through the state file itself when mktemp fails
        "asio-starve-alert-watchdog.sh", "avsync-heartbeat-alert-watchdog.sh",
        "cadence-alert-watchdog.sh", "cg-bridge-alert-watchdog.sh",
        "frozen-input-alert-watchdog.sh", "grabber-stuck-alert-watchdog.sh",
        "imag-obs-alert-watchdog.sh", "imag-power-envelope-alert-watchdog.sh",
        "obs-session-watchdog.sh", "optical-chain-alert-watchdog.sh",
        "splitter-port-alert-watchdog.sh",
        # read-first like the lib, but printf formats with a literal newline
        "network-reach-alert-watchdog.sh", "obs-liveness-watchdog.sh",
        # a fixed temp path
        "obs-burn-reconcile-watchdog.sh",
    },
    "read_state_field": {"ndi-portmap-alert-watchdog.sh", "netcfg-drift-alert-watchdog.sh"},
    # each with its own *_FETCH_CMD test seam
    "fetch_bundle_json": {"audio-mixer-alert-watchdog.sh", "genlock-lock-alert-watchdog.sh",
                          "vb-matrix-alert-watchdog.sh"},
}

_DEF_RE = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\s*\(\)\s*\{\s*$")


def _function_blocks(text):
    """{name: block text} for every column-0 `name() {` ... `}` definition."""
    lines = text.split("\n")
    out, i = {}, 0
    while i < len(lines):
        m = _DEF_RE.match(lines[i])
        if m:
            j = i + 1
            while j < len(lines) and lines[j] != "}":
                j += 1
            out[m.group(1)] = "\n".join(lines[i:j + 1])
            i = j + 1
        else:
            i += 1
    return out


def _declare_f(block, name, tmp_path):
    """bash's own normalized text of one function (comments dropped, one canonical layout)."""
    f = tmp_path / f"fn_{name}.sh"
    f.write_text(block + "\n")
    r = subprocess.run(["bash", "-c", f'. "{f}"; declare -f {name}'], capture_output=True, text=True,
                       timeout=30)
    assert r.returncode == 0 and r.stdout, r.stderr
    return r.stdout


def _watchdogs():
    return sorted(p for p in _SCRIPTS.glob("*watchdog*.sh") if p.name != "avsync-watchdog-install.sh")


def _run(tmp_path, body):
    script = tmp_path / "case.sh"
    script.write_text("set -uo pipefail\n" f'. "{_LIB}"\n' + body)
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, timeout=30)


def test_lib_defines_exactly_the_shared_helpers_and_nothing_else():
    text = _LIB.read_text()
    assert set(_function_blocks(text)) == LIB_FUNCS
    code = [l for l in text.split("\n") if l.strip() and not l.lstrip().startswith("#")]
    # every non-comment line sits inside a function block: nothing runs at source time
    inside, depth_lines = False, 0
    for line in code:
        if _DEF_RE.match(line):
            inside = True
            continue
        if line == "}":
            inside = False
            continue
        assert inside, f"top-level statement in the source-only lib: {line!r}"
        depth_lines += 1
    assert depth_lines > 0
    assert not any(re.match(r"\s*set\s+[-+]", l) for l in code), "the lib must never change shell options"


def test_sourcing_the_lib_leaves_the_callers_shell_options_alone(tmp_path):
    r = _run(tmp_path, 'echo "opts=$-"; set -o | grep -E "^(errexit|nounset|pipefail)\\b" | tr -s " \\t" " "\n')
    assert r.returncode == 0, r.stderr
    assert "errexit off" in r.stdout
    assert "nounset on" in r.stdout and "pipefail on" in r.stdout


def test_write_then_read_keeps_one_line_per_key(tmp_path):
    state = tmp_path / "sub" / "w.state"
    r = _run(tmp_path, f'STATE_FILE="{state}"\n'
             'write_state_field alpha 1\nwrite_state_field beta two\nwrite_state_field alpha 3\n'
             'echo "alpha=$(read_state_field alpha x)"\necho "beta=$(read_state_field beta x)"\n'
             'echo "gamma=$(read_state_field gamma dflt)"\n')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["alpha=3", "beta=two", "gamma=dflt"]
    assert sorted(state.read_text().split("\n")[:-1]) == ["alpha=3", "beta=two"]


def test_read_on_a_missing_state_file_returns_the_default(tmp_path):
    r = _run(tmp_path, f'STATE_FILE="{tmp_path}/absent.state"\necho "v=$(read_state_field k 7)"\n')
    assert r.stdout.strip() == "v=7"


def test_mktemp_failure_fallback_keeps_the_other_keys(tmp_path):
    # the read-first copy captures the other keys BEFORE any file is opened for writing, so the
    # fallback write through STATE_FILE itself (mktemp failing) never drops them.
    state = tmp_path / "f.state"
    fakebin = tmp_path / "bin"
    fakebin.mkdir()
    (fakebin / "mktemp").write_text("#!/bin/sh\nexit 1\n")
    (fakebin / "mktemp").chmod(0o755)
    r = _run(tmp_path, f'STATE_FILE="{state}"\nwrite_state_field keep 1\nwrite_state_field other 2\n'
             f'PATH="{fakebin}:$PATH"\nwrite_state_field other 9\ncat "$STATE_FILE"\n')
    assert r.returncode == 0, r.stderr
    assert sorted(r.stdout.split()) == ["keep=1", "other=9"]


def test_throttle_clears_reset_the_confirm_sig_passes_triples(tmp_path):
    state = tmp_path / "t.state"
    r = _run(tmp_path, f'STATE_FILE="{state}"\n'
             'for k in confirm alert_sig alert_passes confirm_box1 alert_sig_box1 alert_passes_box1 '
             'confirm_src alert_sig_src alert_passes_src alerted_box1; do write_state_field "$k" 5; done\n'
             'clear_throttle\nclear_box_throttle box1\nclear_source_throttle src\ncat "$STATE_FILE"\n')
    got = dict(l.split("=", 1) for l in r.stdout.split("\n") if l)
    for sfx in ("", "_box1", "_src"):
        assert got["confirm" + sfx] == "0"
        assert got["alert_sig" + sfx] == ""
        assert got["alert_passes" + sfx] == "0"
    assert got["alerted_box1"] == "5", "the recovery latch is never touched by a throttle clear"


def test_source_key_keeps_two_names_that_sanitize_alike_apart(tmp_path):
    r = _run(tmp_path, 'source_key "NDI 2ME PGM"; echo; source_key "NDI-2ME-PGM"; echo\n')
    a, b = r.stdout.split()
    assert a.startswith("NDI_2ME_PGM_") and b.startswith("NDI_2ME_PGM_")
    assert a != b
    assert re.fullmatch(r"[A-Za-z0-9_]+", a)


def test_netreach_box_alerted_reads_the_network_reach_latch(tmp_path):
    nr = tmp_path / "nr.state"
    nr.write_text("alerted_strih-lx=1\nalerted_stream=0\n")
    r = _run(tmp_path, f'NETREACH_STATE_FILE="{nr}"\n'
             'for b in strih-lx stream resolume; do printf "%s=%s " "$b" "$(netreach_box_alerted "$b")"; done\n'
             f'NETREACH_STATE_FILE="{tmp_path}/none.state"\nprintf "absent=%s" "$(netreach_box_alerted stream)"\n')
    assert r.stdout.split() == ["strih-lx=1", "stream=0", "resolume=0", "absent=0"]


def test_no_watchdog_carries_the_lib_code_again(tmp_path):
    # compared by `declare -f`, so a copy that differs only in comments or layout is caught too, in
    # every watchdog -- also one that does not source the lib (it should source it instead)
    lib = _function_blocks(_LIB.read_text())
    lib_norm = {n: _declare_f(b, n, tmp_path) for n, b in lib.items()}
    checked = 0
    for wd in _watchdogs():
        for name, block in _function_blocks(wd.read_text()).items():
            if name not in LIB_FUNCS:
                continue
            checked += 1
            assert _declare_f(block, name, tmp_path) != lib_norm[name], (
                f"{wd.name}'s {name}() is the lib's code again -- delete the copy and source the lib")
    assert checked >= len(set().union(*ALLOWED_OVERRIDES.values()))


def test_a_sourcing_watchdog_keeps_only_documented_local_copies():
    sourcing = []
    for wd in _watchdogs():
        text = wd.read_text()
        if _SOURCE_LINE not in text:
            continue
        sourcing.append(wd.name)
        src_at = text.index(_SOURCE_LINE)
        for name, block in _function_blocks(text).items():
            if name not in LIB_FUNCS:
                continue
            assert wd.name in ALLOWED_OVERRIDES.get(name, set()), (
                f"{wd.name} keeps its own {name}() while sourcing the lib -- delete it (the lib "
                f"provides it) or, if its code genuinely differs, list it in ALLOWED_OVERRIDES and "
                f"the lib header")
            assert text.index(block) > src_at, (
                f"{wd.name} defines {name}() before sourcing the lib, so the lib copy would win")
    # every documented override is still a live local copy in a sourcing watchdog
    for name, wds in ALLOWED_OVERRIDES.items():
        for wd in wds:
            assert wd in sourcing, f"{wd} no longer sources the lib -- update ALLOWED_OVERRIDES"
            assert name in _function_blocks((_SCRIPTS / wd).read_text()), (
                f"{wd} no longer defines {name}() -- drop it from ALLOWED_OVERRIDES and the lib header")
    assert len(sourcing) >= 20


def test_every_caller_of_a_lib_helper_can_resolve_it():
    call_re = {n: re.compile(r"(?<![A-Za-z0-9_])" + n + r"(?![A-Za-z0-9_])") for n in LIB_FUNCS}
    for wd in _watchdogs():
        text = wd.read_text()
        defs = _function_blocks(text)
        body = "\n".join(l for l in text.split("\n") if not l.lstrip().startswith("#"))
        for name, rx in call_re.items():
            if name in defs or not rx.search(body):
                continue
            assert _SOURCE_LINE in text, f"{wd.name} calls {name} but neither defines it nor sources the lib"
