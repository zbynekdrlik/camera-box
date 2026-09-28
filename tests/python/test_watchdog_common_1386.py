"""issue 1386 item 3 -- the ONE shared glue of the dev1 alert watchdogs.

scripts/lib/watchdog-common.sh holds every helper that was the same code (bash `declare -f`) in at
least three watchdogs: read_state_field, write_state_field, clear_throttle, clear_box_throttle,
clear_source_throttle, source_key, netreach_box_alerted, fetch_bundle_json. A helper whose code
differed stayed local; where such a local copy shares a lib name it is defined AFTER the source
line, so the local copy is the one that runs. The follow-up (slice C) removed the differing copies
of three of them: the one write_state_field (never drops state, loud, -e safe), recovery_latch_fires
(the recovery decision that lived under three names) and fetch_bundle_json with a per-call
*_FETCH_CMD seam. Only read_state_field keeps local variants. This file pins:

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
    "recovery_latch_fires",
}

# The local copies the lib header documents: name -> watchdogs that keep their own (different) code.
ALLOWED_OVERRIDES = {
    # write_state_field has NO override: every watchdog uses the lib's one write (see
    # test_the_state_write_lives_only_in_the_lib)
    "read_state_field": {"ndi-portmap-alert-watchdog.sh", "netcfg-drift-alert-watchdog.sh",
                         "avsync-lineup-alert-watchdog.sh", "vban-rate-alert-watchdog.sh"},
    # fetch_bundle_json has NO override: the *_FETCH_CMD seams of audio-mixer, genlock-lock and
    # vb-matrix are the lib's SEAM_VAR argument (see the fetch seam tests below)
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
    # the fallback is not atomic, so it is reported (a journal line), never silent
    assert "mktemp failed" in r.stderr and str(state) in r.stderr


def _stub(fakebin, name, body):
    fakebin.mkdir(exist_ok=True)
    f = fakebin / name
    f.write_text("#!/bin/sh\n" + body)
    f.chmod(0o755)


def test_a_failed_temp_write_never_replaces_the_state_file(tmp_path):
    # The temp file exists but the write into it fails (a full disk: mktemp needs only an inode,
    # the write needs a block). Renaming that temp file over the state file would drop every key --
    # every alerted_ recovery latch and confirm counter. A symlink to /dev/full reproduces the
    # failing write for any user, root included.
    state = tmp_path / "w.state"
    state.write_text("alerted_strih-lx=1\nconfirm_strih-lx=2\n")
    fakebin = tmp_path / "bin"
    _stub(fakebin, "mktemp", 't="${1%XXXXXX}full"; ln -sf /dev/full "$t"; printf "%s\\n" "$t"\n')
    r = _run(tmp_path, f'STATE_FILE="{state}"\nPATH="{fakebin}:$PATH"\n'
             'write_state_field confirm_strih-lx 3\necho "rc=$?"\n')
    assert r.stdout.strip() == "rc=0", "a failed state write must never end the watchdog's pass"
    assert state.is_file() and not state.is_symlink(), "the temp file was renamed over the state file"
    assert state.read_text() == "alerted_strih-lx=1\nconfirm_strih-lx=2\n", "the old state must stay"
    assert not list(tmp_path.glob("w.state.*")), "the failed temp file must be removed"
    assert "state write failed" in r.stderr and "confirm_strih-lx" in r.stderr


def test_an_unreadable_state_file_is_never_rewritten_from_nothing(tmp_path):
    # grep exits 2 when it cannot read the file: the other keys are unknown, so rewriting the file
    # would keep only the one key being written. The write is skipped and reported instead.
    state = tmp_path / "u.state"
    state.write_text("alerted_stream=1\nconfirm_stream=4\n")
    fakebin = tmp_path / "bin"
    _stub(fakebin, "grep", "exit 2\n")
    r = _run(tmp_path, f'STATE_FILE="{state}"\nPATH="{fakebin}:$PATH"\n'
             'write_state_field confirm_stream 5\necho "rc=$?"\n')
    assert r.stdout.strip() == "rc=0"
    assert state.read_text() == "alerted_stream=1\nconfirm_stream=4\n"
    assert "state write failed" in r.stderr and "confirm_stream" in r.stderr


def test_write_is_errexit_safe(tmp_path):
    # Rewriting the only key in the file makes `grep -v` match nothing (exit 1); under a caller's
    # `set -e` that exit must not end the script. Every other path must be -e safe too.
    state = tmp_path / "e.state"
    script = tmp_path / "errexit.sh"
    script.write_text("set -euo pipefail\n" f'. "{_LIB}"\nSTATE_FILE="{state}"\n'
                      "write_state_field only 1\nwrite_state_field only 2\n"
                      "clear_throttle\nwrite_state_field alert_sig x\necho SURVIVED\n")
    r = subprocess.run(["bash", str(script)], capture_output=True, text=True, timeout=30)
    assert r.stdout.strip() == "SURVIVED", r.stderr
    got = dict(l.split("=", 1) for l in state.read_text().split("\n") if l)
    assert got == {"only": "2", "confirm": "0", "alert_passes": "0", "alert_sig": "x"}


def test_a_successful_write_keeps_the_other_keys_in_order_and_is_silent(tmp_path):
    state = tmp_path / "o.state"
    state.write_text("a=1\nb=2\nc=3\n")
    r = _run(tmp_path, f'STATE_FILE="{state}"\nwrite_state_field b 9\n')
    assert r.returncode == 0 and r.stderr == ""
    assert state.read_text() == "a=1\nc=3\nb=9\n"
    assert not list(tmp_path.glob("o.state.*"))


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


def test_the_state_write_lives_only_in_the_lib():
    # one write behaviour for every watchdog (issue 1386): no script keeps its own copy of it
    # any definition form: column 0 or indented, `name()` or `function name` (a CALL does not match)
    rx = re.compile(r"^\s*(?:function\s+write_state_field\b|write_state_field\s*\(\s*\))", re.M)
    owners = sorted(str(p.relative_to(_SCRIPTS)) for p in _SCRIPTS.rglob("*.sh") if rx.search(p.read_text()))
    assert owners == ["lib/watchdog-common.sh"], owners


# The recovery decision the watchdogs used to carry under three names (net_reach_recovery_decision_local
# in audio-lag and vb-matrix, genlock_lock_recovery_decision, render-freeze's recovery_now).
_OLD_RECOVERY = "[ \"${1:-0}\" = \"1\" ] && printf '1' || printf '0'"
_OLD_RECOVERY_NAMES = ("net_reach_recovery_decision_local", "genlock_lock_recovery_decision", "recovery_now")


def test_recovery_latch_fires_answers_exactly_like_the_old_copies(tmp_path):
    inputs = ["1", "0", "", "2", "01", " 1", "1 ", "yes", "true"]
    body = "old_recovery() { " + _OLD_RECOVERY + "; }\n"
    for arg in inputs:
        body += f'printf "%s|%s|%s\\n" {arg!r} "$(old_recovery {arg!r})" "$(recovery_latch_fires {arg!r})"\n'
    body += 'printf "noarg|%s|%s\\n" "$(old_recovery)" "$(recovery_latch_fires)"\n'
    r = _run(tmp_path, body)
    assert r.returncode == 0, r.stderr
    rows = [l.split("|") for l in r.stdout.strip().split("\n")]
    assert len(rows) == len(inputs) + 1
    for arg, old_out, new_out in rows:
        assert old_out == new_out, (arg, old_out, new_out)
    assert [row[2] for row in rows] == ["1"] + ["0"] * len(inputs)


def test_the_recovery_decision_lives_only_in_the_lib():
    for p in sorted(_SCRIPTS.rglob("*.sh")):
        if p == _LIB:
            continue
        text = p.read_text()
        assert _OLD_RECOVERY not in text, f"{p.name} carries its own copy of the recovery decision"
        for name in _OLD_RECOVERY_NAMES:
            assert not re.search(r"(?<![A-Za-z0-9_])" + name + r"(?![A-Za-z0-9_])", text), (
                f"{p.name} still names {name} -- call recovery_latch_fires from the lib")
    callers = [p.name for p in _watchdogs() if "recovery_latch_fires" in p.read_text()]
    assert set(callers) >= {"audio-lag-alert-watchdog.sh", "vb-matrix-alert-watchdog.sh",
                            "genlock-lock-alert-watchdog.sh", "render-freeze-alert-watchdog.sh"}


# -- fetch_bundle_json and the per-watchdog *_FETCH_CMD seam ------------------------------------

def _fetch_env(tmp_path, curl_body='{"from":"curl"}'):
    fakebin = tmp_path / "bin"
    _stub(fakebin, "curl", f"printf '%s' '{curl_body}'\n")
    return (f'PATH="{fakebin}:$PATH"\nCURL_TIMEOUT=5\nBUNDLE_PORT=8899\nBUNDLE_PATH=/bundle-state.json\n')


def _fetch(tmp_path, body):
    return _run(tmp_path, _fetch_env(tmp_path) + body)


def test_fetch_without_a_seam_reads_the_box_with_curl(tmp_path):
    r = _fetch(tmp_path, 'fetch_bundle_json 10.0.0.1; echo " rc=$?"\n')
    assert r.stdout == '{"from":"curl"} rc=0\n', r.stderr


def test_a_seam_is_used_only_when_the_caller_names_it(tmp_path):
    # An exported seam variable of ANOTHER watchdog never redirects a fetch that does not name it.
    _stub(tmp_path / "fx", "fetch", 'printf \'{"from":"seam","ip":"%s"}\' "$1"\n')
    body = (f'export AUDIO_MIXER_FETCH_CMD="{tmp_path}/fx/fetch"\n'
            'fetch_bundle_json 10.0.0.1; echo " rc=$?"\n'
            'fetch_bundle_json 10.0.0.1 GENLOCK_LOCK_FETCH_CMD; echo " rc=$?"\n'
            'fetch_bundle_json 10.0.0.2 AUDIO_MIXER_FETCH_CMD; echo " rc=$?"\n')
    r = _fetch(tmp_path, body)
    assert r.stdout.split("\n")[:3] == ['{"from":"curl"} rc=0', '{"from":"curl"} rc=0',
                                        '{"from":"seam","ip":"10.0.0.2"} rc=0'], (r.stdout, r.stderr)


def test_a_seam_value_is_one_executable_path_or_a_command_split_into_words(tmp_path):
    spaced = tmp_path / "a dir"
    _stub(spaced, "fetch me", 'printf \'{"whole":"%s"}\' "$1"\n')
    script = tmp_path / "fixture.sh"
    script.write_text('printf \'{"split":"%s"}\' "$1"\n')          # not executable: run via bash
    body = (f'X_FETCH_CMD="{spaced}/fetch me"\nfetch_bundle_json 1.2.3.4 X_FETCH_CMD; echo " rc=$?"\n'
            f'X_FETCH_CMD="bash {script}"\nfetch_bundle_json 1.2.3.5 X_FETCH_CMD; echo " rc=$?"\n')
    r = _fetch(tmp_path, body)
    assert r.stdout.split("\n")[:2] == ['{"whole":"1.2.3.4"} rc=0', '{"split":"1.2.3.5"} rc=0'], r.stderr


def test_a_failing_or_non_json_seam_reads_as_unreachable_and_leading_space_is_stripped(tmp_path):
    _stub(tmp_path / "fx", "fail", "printf '{\"x\":1}'; exit 3\n")
    _stub(tmp_path / "fx", "html", "printf '<html>'\n")
    _stub(tmp_path / "fx", "spaced", "printf '  \\n {\"ok\":1}'\n")
    body = ''.join(f'X_FETCH_CMD="{tmp_path}/fx/{n}"\nfetch_bundle_json 1.1.1.1 X_FETCH_CMD; '
                   'echo " rc=$?"\n' for n in ("fail", "html", "spaced"))
    body += 'X_FETCH_CMD="   "\nfetch_bundle_json 1.1.1.1 X_FETCH_CMD; echo " rc=$?"\n'
    r = _fetch(tmp_path, body)
    assert r.stdout.split("\n")[:4] == [" rc=1", " rc=1", '{"ok":1} rc=0', " rc=1"], (r.stdout, r.stderr)


def test_a_bad_seam_name_is_a_loud_skip(tmp_path):
    r = _fetch(tmp_path, 'fetch_bundle_json 1.1.1.1 "X;rm"; echo " rc=$?"\n')
    assert r.stdout.strip() == "rc=1" and "not a shell variable name" in r.stderr


def test_the_seam_watchdogs_pass_their_own_seam_name():
    for wd, var in (("audio-mixer-alert-watchdog.sh", "AUDIO_MIXER_FETCH_CMD"),
                    ("genlock-lock-alert-watchdog.sh", "GENLOCK_LOCK_FETCH_CMD"),
                    ("vb-matrix-alert-watchdog.sh", "VB_MATRIX_FETCH_CMD")):
        text = (_SCRIPTS / wd).read_text()
        assert "fetch_bundle_json" not in _function_blocks(text), f"{wd} keeps its own fetch copy"
        calls = re.findall(r"fetch_bundle_json \"\$ip\"(?: (\w+))?\)", text)
        assert calls and all(c == var for c in calls), (wd, calls)


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
