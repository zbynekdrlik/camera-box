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


def _shell_lines(text):
    """(line number, line) of TEXT's shell code: a heredoc's body (python, a remote script) is data."""
    end = None
    for n, line in enumerate(text.splitlines(), 1):
        if end is not None:
            if line.strip() == end:
                end = None
            continue
        m = HEREDOC.search(re.sub(r"(^|\s)#.*$", "", line))
        if m:
            end = m.group(2)
        yield n, line


def _bodies(texts):
    bodies = {}
    for text in texts.values():
        cur = None
        for _, line in _shell_lines(text):
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


def _subshell_calls(texts, graders):
    """`file:line: name` for each grader called where its counter update is lost."""
    hits = []
    for path, text in texts.items():
        in_while = 0
        for n, raw in _shell_lines(text):
            if raw.lstrip().startswith("#") or DEF.match(raw):
                continue
            line = _code(raw)
            if re.search(r"(?<!\|)\|\s*while\b", line):
                in_while += 1
            for g in sorted(graders):
                if not _calls(line, g):
                    continue
                for seg in re.split(r";|&&|\|\|", line):
                    m = _calls(seg, g)
                    if not m:
                        continue
                    before = seg[:m.start() + len(m.group(0))]
                    if PIPE.search(seg) or "$(" in before or "<(" in before or "`" in before or in_while:
                        hits.append("%s:%d: %s" % (path, n, g))
                        break
            if in_while and re.match(r"\s*done\b", line):
                in_while -= 1
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


def test_the_sweep_leaves_the_shapes_that_keep_the_count():
    assert not _shape('caller() {\n  grade_x <<<"$t"\n}\n')
    assert not _shape('caller() {\n  wrap || true\n}\n')
    assert not _shape('caller() {\n  if [ -n "$x" ]; then grade_x; fi\n}\n')
    assert not _shape('caller() {\n  while read -r l; do grade_x; done < "$f"\n}\n')
    assert not _shape('caller() {\n  echo "grade_x | x"   # grade_x | prose\n}\n')
    assert not _shape('caller() {\n  bad=0; [ "$bad" = 0 ] || x=1; n=$((bad + 1))\n}\n')
    assert not _shape('caller() {\n  python3 - <<PY | sort\nbad = []\nprint(1) | 2\nPY\n}\n')


def test_no_verify_row_printer_runs_in_a_subshell():
    files = sorted((SCRIPTS / "lib").glob("*.sh")) + [SCRIPTS / "verify-strih.sh"]
    texts = {str(f.relative_to(REPO)): f.read_text(errors="replace") for f in files}
    graders = _graders(texts)
    assert {"strih_dante_media_clock_grade", "strih_intercom_hub_media_clock_grade"} <= graders
    assert len(graders) > 30, sorted(graders)
    hits = _subshell_calls(texts, graders)
    assert hits == [], "a grader in a subshell loses its FAIL count:\n" + "\n".join(hits)
