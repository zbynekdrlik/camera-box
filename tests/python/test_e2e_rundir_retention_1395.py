"""issue 1395 -- bounded retention of the dev1 E2E run dirs (/tmp/recording-e2e-<RUN_ID>).

recording-e2e.sh writes every run into its own /tmp/recording-e2e-<RUN_ID> and nothing ever removed
an older one (268 dirs / 5.9 GB on 23.9.2026 on the shared dev1 disk). The dir must OUTLIVE the run
(full-path-e2e.yml reads the verdict, uploads the artifacts and derives the failure stage from it
after the script returns; the mining tools read several archived runs), so the fix is a bounded
retention, not a delete-on-exit: scripts/lib/e2e-rundir-retention.sh `e2e_rundir_retention <parent>
<current> [keep]` keeps the current run dir + the newest N other `recording-e2e-<digits>` dirs by
mtime (N = keep, else E2E_RUNDIR_KEEP, else 8), removes the older ones, never follows a symlink,
never touches another name, logs ONE line, and always returns 0 under the caller's
`set -euo pipefail`.

This file pins that behaviour over fake run dirs in a pytest temp parent (never the real /tmp), plus
a static check that recording-e2e.sh sources the lib once and calls the helper right after it
creates OUTDIR. Tier-0: pure python + bash subprocesses (no cargo).
"""
import os
import pathlib
import re
import subprocess
import time

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "e2e-rundir-retention.sh"
_E2E = _ROOT / "scripts" / "recording-e2e.sh"
_SOURCE_STMT = '. "$HERE/lib/e2e-rundir-retention.sh"'
_CALL_STMT = 'e2e_rundir_retention "$(dirname "$OUTDIR")" "$OUTDIR"'
_CONTINUES = "CALLER-CONTINUES"


def _run(args, env_extra=None, path_prefix=None):
    """Source the lib under the caller's real `set -euo pipefail`, call the helper as a BARE statement
    (exactly like recording-e2e.sh does) and prove the caller keeps running after it."""
    quoted = " ".join("'" + str(a).replace("'", "'\\''") + "'" for a in args)
    script = (
        "set -euo pipefail\n"
        f'. "{_LIB}"\n'
        f"e2e_rundir_retention {quoted}\n"
        f'echo "{_CONTINUES} rc=$? opts=$-"\n'
        "set -o | grep -E '^pipefail[[:space:]]+on$'\n"
    )
    env = {k: v for k, v in os.environ.items() if k != "E2E_RUNDIR_KEEP"}
    if env_extra:
        env.update(env_extra)
    if path_prefix:
        env["PATH"] = f"{path_prefix}:{env['PATH']}"
    proc = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=env, timeout=60)
    assert proc.returncode == 0, f"the caller must survive: rc={proc.returncode}\n{proc.stdout}\n{proc.stderr}"
    assert f"{_CONTINUES} rc=0" in proc.stdout, f"the caller must keep running:\n{proc.stdout}\n{proc.stderr}"
    opts = proc.stdout.split(f"{_CONTINUES} rc=0 opts=", 1)[1].split()[0]
    assert "e" in opts and "u" in opts, f"the helper must leave the caller's -e/-u on: {opts}"
    helper_lines = [ln for ln in proc.stdout.splitlines()
                    if not ln.startswith(_CONTINUES) and not ln.startswith("pipefail")]
    return helper_lines, proc


def _age(path, age_s, follow=True):
    t = time.time() - age_s
    os.utime(path, (t, t), follow_symlinks=follow)


def _mkrun(parent, name, age_s, payload=b"x" * 2048):
    d = parent / name
    d.mkdir()
    (d / "verdict.json").write_bytes(payload)
    _age(d, age_s)
    return d


def _names(parent):
    return sorted(p.name for p in parent.iterdir())


def test_keeps_the_current_dir_and_the_newest_n_others_and_removes_the_older_ones(tmp_path):
    parent = tmp_path / "a parent with spaces"
    parent.mkdir()
    # The current run dir is the OLDEST on purpose: it is kept whatever its mtime says.
    current = _mkrun(parent, "recording-e2e-500", 99_000)
    for i in range(10):
        _mkrun(parent, f"recording-e2e-{100 + i}", 3600 * (i + 1))  # 100 newest ... 109 oldest
    lines, _ = _run([parent, current, "3"])
    assert _names(parent) == sorted(
        ["recording-e2e-500", "recording-e2e-100", "recording-e2e-101", "recording-e2e-102"]
    )
    assert len(lines) == 1, f"exactly one log line: {lines}"
    assert "e2e-rundir-retention" in lines[0] and "removed 7" in lines[0], lines


def test_keep_defaults_to_8_and_follows_e2e_rundir_keep(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    current = _mkrun(parent, "recording-e2e-1", 0)
    for i in range(12):
        _mkrun(parent, f"recording-e2e-{1000 + i}", 60 * (i + 1))
    _run([parent, current])
    assert len(_names(parent)) == 1 + 8
    assert {f"recording-e2e-{1000 + i}" for i in range(8)} <= set(_names(parent))
    _run([parent, current], env_extra={"E2E_RUNDIR_KEEP": "2"})
    assert _names(parent) == sorted(["recording-e2e-1", "recording-e2e-1000", "recording-e2e-1001"])


def test_keep_zero_leaves_only_the_current_dir(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    current = _mkrun(parent, "recording-e2e-7", 0)
    _mkrun(parent, "recording-e2e-8", 10)
    _run([parent, current, "0"])
    assert _names(parent) == ["recording-e2e-7"]


def test_never_touches_any_other_name(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    current = _mkrun(parent, "recording-e2e-42", 0)
    _mkrun(parent, "recording-e2e-43", 7200)  # a real old run dir: removed with keep 0
    others = []
    for name in ["recording-e2e-report.png", "recording-e2e-123.log", "recording-e2e-77"]:
        f = parent / name  # regular FILES, one even named like a run dir
        f.write_text("keep me")
        _age(f, 50_000)
        others.append(name)
    for name in ["recording-e2e-abc", "recording-e2e-", "recording-e2e-12x", "xrecording-e2e-5",
                 "recording-e2e-5-old", "e2e-rundir-retention"]:
        _mkrun(parent, name, 50_000)
        others.append(name)
    _run([parent, current, "0"])
    assert _names(parent) == sorted(others + ["recording-e2e-42"])


def test_a_symlink_named_like_a_run_dir_is_never_followed(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "precious.txt").write_text("never delete")
    current = _mkrun(parent, "recording-e2e-1", 0)
    link = parent / "recording-e2e-99"
    link.symlink_to(outside, target_is_directory=True)
    _age(link, 90_000, follow=False)
    # A REMOVED run dir holding a symlink out: rm must unlink the link, never descend into the target.
    old = _mkrun(parent, "recording-e2e-2", 80_000)
    (old / "link-out").symlink_to(outside, target_is_directory=True)
    _age(old, 80_000)
    _run([parent, current, "0"])
    assert link.is_symlink(), "the symlink itself is left alone"
    assert (outside / "precious.txt").read_text() == "never delete"
    assert not old.exists(), "the old real run dir is removed"


def test_an_empty_or_missing_parent_is_a_no_op(tmp_path):
    empty = tmp_path / "empty"
    empty.mkdir()
    lines, _ = _run([empty, empty / "recording-e2e-1"])
    assert len(lines) == 1 and "removed 0" in lines[0], lines
    assert _names(empty) == []
    lines, _ = _run([tmp_path / "missing", tmp_path / "missing" / "recording-e2e-1"])
    assert len(lines) == 1 and "nothing to prune" in lines[0], lines
    assert not (tmp_path / "missing").exists()


def test_no_arguments_at_all_never_kill_the_caller(tmp_path):
    _run([])


def test_an_invalid_keep_removes_nothing(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    current = _mkrun(parent, "recording-e2e-1", 0)
    _mkrun(parent, "recording-e2e-2", 7200)
    for lines, _ in (_run([parent, current], env_extra={"E2E_RUNDIR_KEEP": "lots"}),
                     _run([parent, current, "-1"])):
        assert len(lines) == 1 and "removed nothing" in lines[0], lines
    assert _names(parent) == ["recording-e2e-1", "recording-e2e-2"]


def test_a_failing_du_and_rm_never_abort_the_caller(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    current = _mkrun(parent, "recording-e2e-1", 0)
    _mkrun(parent, "recording-e2e-2", 7200)
    stubs = tmp_path / "stubs"
    stubs.mkdir()
    for tool in ("du", "rm"):
        s = stubs / tool
        s.write_text("#!/bin/sh\necho \"stub $0 refuses\" >&2\nexit 1\n")
        s.chmod(0o755)
    lines, _ = _run([parent, current, "0"], path_prefix=stubs)
    assert _names(parent) == ["recording-e2e-1", "recording-e2e-2"], "rm failed, so the dir stays"
    assert len(lines) == 1 and "could not be removed" in lines[0], lines


# dev1's camera-box runner runs en_US.UTF-8 (its LANG); GitHub's runners run C.UTF-8. Under en_US a
# bash bracket RANGE like [0-9] also matches Arabic-Indic, superscript and fullwidth digits (the
# issue-1302 trap), so only a spelled-out ASCII digit set keeps the delete contract on dev1.
_UNICODE_LOCALE = "en_US.UTF-8"


def test_only_ascii_digit_names_are_run_dirs_under_a_unicode_locale(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    current = _mkrun(parent, "recording-e2e-1", 0)
    _mkrun(parent, "recording-e2e-2", 7200)
    others = ["recording-e2e-\u0661\u0662", "recording-e2e-\u00b2", "recording-e2e-\uff11\uff12"]
    for name in others:
        _mkrun(parent, name, 50_000)
    _run([parent, current, "0"], env_extra={"LC_ALL": _UNICODE_LOCALE})
    assert _names(parent) == sorted(others + ["recording-e2e-1"])


def test_a_non_ascii_digit_keep_removes_nothing_under_a_unicode_locale(tmp_path):
    parent = tmp_path / "p"
    parent.mkdir()
    current = _mkrun(parent, "recording-e2e-1", 0)
    _mkrun(parent, "recording-e2e-2", 7200)
    lines, _ = _run([parent, current], env_extra={"LC_ALL": _UNICODE_LOCALE, "E2E_RUNDIR_KEEP": "\u0665"})
    assert len(lines) == 1 and "removed nothing" in lines[0], lines
    assert _names(parent) == ["recording-e2e-1", "recording-e2e-2"]


def test_every_bracket_set_in_the_lib_spells_its_characters_out():
    """A lint over the lib's code lines: no `x-y` range inside a bracket expression (locale-bound)."""
    bad = []
    for no, line in enumerate(_LIB.read_text().splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        for m in re.finditer(r"\[[!^]?([^\[\]]*)\]", line):
            if re.search(r"\w-\w", m.group(1)):
                bad.append(f"{no}: {line.strip()}")
    assert not bad, "spell digit sets out ([0123456789]), never a range:\n" + "\n".join(bad)


def test_the_lib_is_source_only():
    text = _LIB.read_text()
    assert not re.search(r"^\s*set\s+-[a-zA-Z]*[eu]", text, re.M), \
        "a sourced lib never changes the caller's shell options"
    assert re.search(r"^e2e_rundir_retention\(\) \{$", text, re.M)


def test_recording_e2e_sources_the_lib_once_and_calls_it_right_after_creating_outdir():
    s = _E2E.read_text()
    assert s.count(_SOURCE_STMT) == 1
    src_at = s.index(_SOURCE_STMT)
    assert s.rfind("# shellcheck source=scripts/lib/e2e-rundir-retention.sh", 0, src_at) != -1
    assert s.count(_CALL_STMT) == 1
    mkdir_at = s.find('mkdir -p "$OUTDIR"')
    call_at = s.index(_CALL_STMT)
    assert src_at < mkdir_at < call_at
    assert call_at - mkdir_at < 600, "the prune runs right after OUTDIR exists, before any rig work"
