"""Issue 1485 (4.11.2026, the in-process PTP switch): verify-strih printed
`FAIL (intercom-dante) media clock: source=Service ...` and then `=== verify-strih.sh: ALL CLEAR ===`.

The hub's media clock grade piped the journal into `strih_dante_media_clock_grade`, so the grader ran in a
SUBSHELL: its `bad` printed the row and counted it in a copy of verify-strih's FAILS that was thrown away.

Pinned here:
  * the row: a FAIL of `strih_intercom_hub_media_clock_grade` reaches the CALLER's counter;
  * the sweep: no function that reaches verify-strih's `bad` (directly or through another function) is
    called inside a pipeline, a command / process substitution or a `| while` body, in any lib verify-strih
    can source or in verify-strih.sh itself. The scanner is proven on synthetic shapes first.
"""
import os
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
LEAKY = ("STRIH_DANTE_PTP_INTERCOM_PIN", "STRIH_DANTE_PTP_OBS_PIN", "STRIH_HUB_JOURNAL_FILE",
         "STRIH_BOXES_DIR", "STRIH_HUB_INVOCATION_ID", "STRIH_HUB_JOURNAL_EUID", "STRIH_HUB_JOURNAL_GROUPS")
# verify-strih.sh's own three row printers, byte for byte in behaviour: bad counts.
ROWS = ('FAILS=0\nok() { echo "OK $*"; }; bad() { echo "BAD $*"; FAILS=$((FAILS+1)); }; '
        'note() { echo "NOTE $*"; }\n')


def _bash(body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s%s' % (PROVISION, ROWS, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _pins(tmp):
    ic, ob = tmp / "intercom.pin", tmp / "obs.pin"
    ic.write_text("INTERCOM_RELEASE_TAG=v9.9.9\nINTERCOM_RELEASE_DSTACK_VERSION=v0.6.25\n")
    ob.write_text("OBS_RELEASE_TAG=v9.9.9\nOBS_RELEASE_DSTACK_VERSION=v0.6.25\n")
    return {"STRIH_DANTE_PTP_INTERCOM_PIN": str(ic), "STRIH_DANTE_PTP_OBS_PIN": str(ob)}


def test_a_hub_media_clock_fail_counts_in_the_callers_summary(tmp_path):
    j = tmp_path / "journal.txt"
    j.write_text("INFO dstack::clock::linked: media clock source source=Service from=None\n")
    env = dict(_pins(tmp_path), STRIH_HUB_JOURNAL_FILE=str(j))
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade\necho "FAILS=$FAILS"',
              env=env)
    assert r.returncode == 0, r.stderr
    assert "BAD (intercom-dante) media clock: source=Service" in r.stdout, r.stdout
    assert r.stdout.strip().splitlines()[-1] == "FAILS=1", r.stdout


def test_a_hub_media_clock_ok_counts_nothing(tmp_path):
    j = tmp_path / "journal.txt"
    j.write_text("INFO dstack::clock::linked: media clock source source=Internal from=Service\n")
    env = dict(_pins(tmp_path), STRIH_HUB_JOURNAL_FILE=str(j))
    r = _bash('strih_box_load strih-snv >/dev/null\nstrih_intercom_hub_media_clock_grade\necho "FAILS=$FAILS"',
              env=env)
    assert r.stdout.strip().splitlines()[-1] == "FAILS=0", r.stdout + r.stderr


# --- the sweep ----------------------------------------------------------------------------------------------

DEF = re.compile(r"^([A-Za-z_]\w*)\s*\(\)\s*\{")
PIPE = re.compile(r"(?<!\|)\|(?![|&])")


def _calls(text, name):
    """A call of NAME at command position (never `$name`, `name=`, `${name}` or a longer word)."""
    return re.search(r"(?:^|[;&|({!)]|\$\(|<\(|\bthen\b|\bdo\b|\belse\b|\btime\b)\s*" + re.escape(name)
                     + r"(?=\s|$|;|\))", text)


def _code(line):
    """LINE as code: quoted prose, arithmetic and the trailing comment dropped, so a word in a message or
    `$((bad + 1))` never reads as a call; a `$(...)` inside double quotes is kept (it runs)."""
    s = re.sub(r"\$?\(\([^()]*(\([^()]*\)[^()]*)*\)\)", "0", line)
    s = re.sub(r"'[^']*'", "''", s)
    s = re.sub(r'"(?:\\.|[^"\\])*"',
               lambda m: '""' + " ".join(re.findall(r"\$\([^()]*\)|`[^`]*`", m.group(0))), s)
    return re.sub(r"(^|\s)#.*$", "", s)


HEREDOC = re.compile(r"(?<!<)<<-?\s*(['\"]?)([A-Za-z_]\w*)\1")


def _quote_scan(line, stack):
    """Advance the quote / command-substitution STACK over LINE (a tiny shell lexer: 'N' code, 'S' single
    quotes, 'D' double quotes, ['C', depth] inside `$(`). A comment ends the line in code."""
    i, prev = 0, " "
    while i < len(line):
        c, top = line[i], stack[-1]
        kind = top[0] if isinstance(top, list) else top
        if kind == "S":
            if c == "'":
                stack.pop()
        elif kind == "D":
            if c == "\\":
                i += 1
            elif c == '"':
                stack.pop()
            elif line.startswith("$(", i) and not line.startswith("$((", i):
                stack.append(["C", 0])
                i += 1
        else:   # code: N or a command substitution
            if c == "\\":
                i += 1
            elif c == "#" and prev in " \t;(|&":
                break
            elif c == "'":
                stack.append("S")
            elif c == '"':
                stack.append("D")
            elif line.startswith("$(", i) and not line.startswith("$((", i):
                stack.append(["C", 0])
                i += 1
            elif kind == "C" and c == "(":
                top[1] += 1
            elif kind == "C" and c == ")":
                if top[1] == 0:
                    stack.pop()
                else:
                    top[1] -= 1
        prev = c
        i += 1


def _shell_lines(text):
    """(line number, line, inside a multi-line `$(`) of TEXT's shell code: a heredoc's body and the continuation lines of a multi-line
    single-quoted string (python3 -c '...', a remote script) are data."""
    end, stack = None, ["N"]
    for n, line in enumerate(text.splitlines(), 1):
        if end is not None:
            if line.strip() == end:
                end = None
            continue
        in_data = stack[-1] == "S"
        in_sub = any(isinstance(f, list) for f in stack)
        _quote_scan(line, stack)
        if in_data:
            continue
        m = HEREDOC.search(re.sub(r"(^|\s)#.*$", "", line))
        if m:
            end = m.group(2)
        yield n, line, in_sub


def _bodies(texts):
    bodies = {}
    for text in texts.values():
        cur = None
        for _, line, _ in _shell_lines(text):
            m = DEF.match(line)
            if m:
                cur = m.group(1)
                bodies.setdefault(cur, []).append(line[m.end():])
                if line.rstrip().endswith("}") and line.count("{") == line.count("}"):
                    cur = None
                continue
            if line.startswith("}"):
                cur = None
            elif cur:
                bodies[cur].append(line)
    return bodies


def _graders(texts, root="bad"):
    """Every function that reaches ROOT, directly or through another function of TEXTS."""
    bodies = _bodies(texts)
    found = {root}
    changed = True
    while changed:
        changed = False
        for fn, lines in bodies.items():
            if fn not in found and any(_calls(_code(l), g) for l in lines for g in found):
                found.add(fn)
                changed = True
    return found


LOSE_AFTER = re.compile(r"(?<!\|)\|(?![|&])|(?<![&>|])&(?![&>])")


def _lost_lines(lines):
    """Line numbers whose commands run in a subshell because of a BLOCK around them: a `( ... )` group, a
    loop or `{ ... }` group whose closing line is piped or backgrounded, a `... | while` loop, a line inside
    a `$(` that spans lines."""
    lost, stack = set(), []
    for n, line, in_sub in lines:
        if in_sub:
            lost.add(n)
        closer = re.match(r"\s*(done|\}|\))(.*)$", line)
        if closer and stack:
            want = {"done": "do", "}": "{", ")": "("}[closer.group(1)]
            while stack and stack[-1][0] != want:
                stack.pop()
            if stack:
                kind, start, lost_already = stack.pop()
                rest = re.sub(r"<\(.*\)", "", closer.group(2))   # `done < <(producer)` feeds THIS shell
                if lost_already or LOSE_AFTER.search(rest):
                    lost.update(range(start, n + 1))
            continue
        if re.search(r"(?:^|[;\s])do\s*$", line):
            stack.append(("do", n, bool(re.search(r"(?<!\|)\|\s*(while|until|for)\b", line))))
        elif re.search(r"(?:^|[;&|]|\bthen|\bdo|\belse)\s*\{\s*$", line):
            stack.append(("{", n, False))
        elif re.search(r"(?:^|[;&|]|\bthen|\bdo|\belse)\s*\(\s*$", line):
            stack.append(("(", n, True))
    return lost


def _subshell_calls(texts, graders):
    """`file:line: name` for each grader called where its counter update is lost."""
    hits = []
    for path, text in texts.items():
        lines = [(n, _code(raw), in_sub) for n, raw, in_sub in _shell_lines(text)
                 if not raw.lstrip().startswith("#") and not DEF.match(raw)]
        lost = _lost_lines(lines)
        for n, line, _ in lines:
            for g in sorted(graders):
                if not _calls(line, g):
                    continue
                # a one-line `{ ...; }` group or `do ...; done` loop whose end is piped or backgrounded
                one = [m for m in re.finditer(r"\{(.*?)\}(.*)$|\bdo\b(.*?)\bdone\b(.*)$", line)
                       if _calls(m.group(1) or m.group(3) or "", g)
                       and LOSE_AFTER.search(re.sub(r"<\(.*\)", "", m.group(2) or m.group(4) or ""))]
                if one:
                    hits.append("%s:%d: %s" % (path, n, g))
                    continue
                for seg in re.split(r";|&&|\|\|", line):
                    m = _calls(seg, g)
                    if not m:
                        continue
                    prefix = seg[:m.end() - len(g)]
                    if (n in lost or LOSE_AFTER.search(seg) or "$(" in prefix or "<(" in prefix or "`" in prefix
                            or re.search(r"(?<![$<(])\(\s*$", prefix)):
                        hits.append("%s:%d: %s" % (path, n, g))
                        break
    return hits


def _shape(body):
    texts = {"lib.sh": "grade_x() {\n  bad \"x\"\n}\nwrap() {\n  grade_x\n}\n" + body}
    return _subshell_calls(texts, _graders(texts))


def test_the_sweep_catches_every_shape_that_loses_the_count():
    assert _shape('caller() {\n  printf x | grade_x\n}\n'), "pipe into a grader"
    assert _shape('caller() {\n  grade_x | tee log\n}\n'), "a grader piped out"
    assert _shape('caller() {\n  v="$(wrap)"\n}\n'), "a transitive grader in a command substitution"
    assert _shape('caller() {\n  cat < <(grade_x)\n}\n'), "process substitution"
    assert _shape('caller() {\n  printf x | while read -r l; do\n    wrap\n  done\n}\n'), "a `| while` body"
    # review round 1: the shapes the first scanner missed
    assert _shape('caller() {\n  ( grade_x )\n}\n'), "an explicit subshell"
    assert _shape('caller() {\n  (\n    grade_x\n  )\n}\n'), "a multi-line subshell"
    assert _shape('caller() {\n  for x in a b; do\n    grade_x\n  done | tee log\n}\n'), "a loop piped out"
    assert _shape('caller() {\n  { grade_x; } | tee log\n}\n'), "a brace group piped out"
    assert _shape('caller() {\n  {\n    wrap\n  } 2>&1 | tee log\n}\n'), "a multi-line brace group piped out"
    assert _shape('caller() {\n  grade_x &\n  wait\n}\n'), "a backgrounded grader"
    assert _shape('caller() {\n  v="$(\n    grade_x\n  )"\n}\n'), "a multi-line command substitution"


def test_the_sweep_leaves_the_shapes_that_keep_the_count():
    assert not _shape('caller() {\n  grade_x <<<"$t"\n}\n')
    assert not _shape('caller() {\n  wrap || true\n}\n')
    assert not _shape('caller() {\n  if [ -n "$x" ]; then grade_x; fi\n}\n')
    assert not _shape('caller() {\n  while read -r l; do grade_x; done < "$f"\n}\n')
    assert not _shape('caller() {\n  while read -r l; do\n    grade_x\n  done < <(producer)\n}\n')
    assert not _shape('caller() {\n  x || {\n    grade_x\n    return 0\n  }\n}\n')
    assert not _shape('caller() {\n  grade_x >&2 2>&1\n}\n')
    assert not _shape("caller() {\n  v=\"$(python3 -c '\nbad = []\nprint(1) | 2\n')\"\n  grade_x\n}\n")
    assert not _shape('caller() {\n  echo "grade_x | x"   # grade_x | prose\n}\n')
    assert not _shape('caller() {\n  bad=0; [ "$bad" = 0 ] || x=1; n=$((bad + 1))\n}\n')
    assert not _shape('caller() {\n  python3 - <<PY | sort\nbad = []\nprint(1) | 2\nPY\n}\n')


def test_no_verify_row_printer_runs_in_a_subshell():
    vs = (SCRIPTS / "verify-strih.sh").read_text()
    # every lib, verify-strih itself and every other script it sources (clock-offset-guard.sh)
    sourced = [SCRIPTS / m for m in re.findall(r'^\. "\$\{HERE\}/([^"/]+\.sh)"', vs, re.M)]
    assert SCRIPTS / "clock-offset-guard.sh" in sourced, sourced
    files = sorted((SCRIPTS / "lib").glob("*.sh")) + [SCRIPTS / "verify-strih.sh"] + sourced
    texts = {str(f.relative_to(REPO)): f.read_text(errors="replace") for f in files}
    graders = _graders(texts)
    assert {"strih_dante_media_clock_grade", "strih_intercom_hub_media_clock_grade"} <= graders
    assert len(graders) > 30, sorted(graders)
    hits = _subshell_calls(texts, graders)
    assert hits == [], "a grader in a subshell loses its FAIL count:\n" + "\n".join(hits)
