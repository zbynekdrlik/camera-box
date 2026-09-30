#!/usr/bin/env python3
"""#1394 -- the shared CI run resolver trusted ONE stale GitHub run listing.

On 30.9.2026 a relay deploy to cam4-cam7 resolved main run 33857572305 (4.9., sha 5ed4e44ad) while
the branch head 1f7e6569b already had its own successful run 36687519583: the filtered runs listing
served a STALE result set whose newest success was 34999288119 (15.9., artifact expired), and
scripts/lib/ci-run-resolve.sh walked it down to the 4.9. run. The same stale pick happened on
25.9.2026. Re-reading the listing seconds later returned the newest run again, so no client-side
sort can fix it: the run is simply absent from the payload.

The fix (design issuecomment-5908942618, Approach 1): anchor on the branch HEAD.
  - read the head sha from the branch ref (a git ref read, strongly consistent);
  - look up the head commit's own runs; a successful one carrying the artifact is the pick;
  - neither the head lookup nor the branch listing has ANY run for the head -> the listing is
    STALE: re-read both (bounded), then fail loud -- never an older run;
  - the head's run in progress / queued / failed / without the artifact -> the old walk (newest
    older success carrying the artifact) with ONE loud line naming the head's state;
  - a gh error reading the head, or the head's runs, fails loud -- never a fallback.
setup-device.sh's two inline lookups now go through the same lib.

Tier-0: stdlib-only, a fake gh (it applies each --jq program with the real jq, like gh's built-in
jq) and a fake sleep -- no network, no rig.
"""
import json
import os
import re
import stat
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
RESOLVE_LIB = os.path.join(REPO, "scripts", "lib", "ci-run-resolve.sh")
SETUP = os.path.join(REPO, "scripts", "setup-device.sh")
DEPLOY_SERVICE = os.path.join(REPO, "scripts", "bkshading-deploy-service.sh")
GH_REPO = "zbynekdrlik/camera-box"
ART = "camera-box-linux-amd64"

# The live 30.9.2026 case (run ids, dates and shas read back from GitHub).
HEAD = "1f7e6569b78c07f1c8fd6f8f5e2dc25dd26d47cb"
HEAD_RUN = {"databaseId": 36687519583, "createdAt": "2026-09-30T08:05:08Z", "status": "completed",
            "conclusion": "success", "headSha": HEAD}
STALE_15_9 = {"databaseId": 34999288119, "createdAt": "2026-09-15T17:07:35Z", "status": "completed",
              "conclusion": "success", "headSha": "616724df613fcf900468313cac76275d129a88a9"}
STALE_4_9 = {"databaseId": 33857572305, "createdAt": "2026-09-04T09:17:04Z", "status": "completed",
             "conclusion": "success", "headSha": "5ed4e44ad7deabe27a925eadc403d6e62c8af60a"}
STALE_LISTING = [STALE_15_9, STALE_4_9]
LIVE_ARTS = {36687519583: [ART], 34999288119: [(ART, True)], 33857572305: [ART]}

# The live head while its own run is still in flight.
HEAD_INPROG = dict(HEAD_RUN, status="in_progress", conclusion="")

# A synthetic head whose own run is not usable, plus an older success that is.
HEAD_B = "b" * 40
OLDER = {"databaseId": 800, "createdAt": "2026-09-29T10:00:00Z", "status": "completed",
         "conclusion": "success", "headSha": "a" * 40}


def _read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def _write_exec(path, body):
    with open(path, "w", encoding="utf-8") as f:
        f.write(body)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def _noncomment(text):
    return "\n".join(ln for ln in text.splitlines() if not ln.lstrip().startswith("#"))


class FakeGh:
    """A fake gh in a temp dir.

    listings / commit_runs: a list of SNAPSHOTS -- read n answers snapshot n (the last one repeats),
    so a stale-then-fresh GitHub listing can be replayed. commit_runs snapshots hold every run the
    head lookup can see; the fake filters them by --commit like the real API.
    arts: {run id: [name | (name, expired)]}.
    """

    def __init__(self, tmp, head, listings, commit_runs, arts, head_error=None, commit_error=None,
                 listing_error=None):
        self.tmp = tmp
        self.log = os.path.join(tmp, "gh.log")
        self.path = os.path.join(tmp, "gh")
        with open(os.path.join(tmp, "branch.json"), "w", encoding="utf-8") as f:
            json.dump({"name": "main", "commit": {"sha": head}}, f)
        for i, snap in enumerate(listings, 1):
            with open(os.path.join(tmp, "listing.%d.json" % i), "w", encoding="utf-8") as f:
                json.dump(snap, f)
        for i, snap in enumerate(commit_runs, 1):
            with open(os.path.join(tmp, "commit.%d.json" % i), "w", encoding="utf-8") as f:
                json.dump(snap, f)
        os.makedirs(os.path.join(tmp, "arts"), exist_ok=True)
        for rid, names in arts.items():
            items = []
            for n in names:
                name, expired = (n if isinstance(n, tuple) else (n, False))
                items.append({"name": name, "expired": expired})
            with open(os.path.join(tmp, "arts", str(rid)), "w", encoding="utf-8") as f:
                json.dump({"total_count": len(items), "artifacts": items}, f)
        body = r'''#!/usr/bin/env bash
T="__T__"
printf 'GH %s\n' "$*" >> "$T/gh.log"
q=""; c=""; b=""
for a in "$@"; do
  case "${prev:-}" in --jq) q="$a" ;; --commit) c="$a" ;; --branch) b="$a" ;; esac
  prev="$a"
done
snap() {  # snap KIND -> the snapshot file for this read of KIND (the last one repeats)
  local n; n=$(( $(cat "$T/$1.count" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$T/$1.count"
  while [ "$n" -gt 1 ] && [ ! -f "$T/$1.$n.json" ]; do n=$((n - 1)); done
  printf '%s\n' "$T/$1.$n.json"
}
if [ "$1" = api ]; then
  case "$2" in
    */branches/*)
      if [ -f "$T/head_error" ]; then cat "$T/head_error" >&2; exit 1; fi
      jq -r "$q" "$T/branch.json"; exit 0 ;;
    */artifacts*)
      id="$(printf '%s' "$2" | sed -n 's#.*/runs/\([0-9]*\)/artifacts.*#\1#p')"
      f="$T/arts/$id"; [ -f "$f" ] || f="$T/no-arts.json"
      [ -f "$T/no-arts.json" ] || echo '{"total_count":0,"artifacts":[]}' > "$T/no-arts.json"
      jq -r "$q" "$f"; exit 0 ;;
  esac
  echo "fake gh: unexpected api $2" >&2; exit 1
fi
if [ "$1 $2" = "run list" ]; then
  if [ -n "$c" ]; then
    if [ -f "$T/commit_error" ]; then cat "$T/commit_error" >&2; exit 1; fi
    f="$(snap commit)"; [ -f "$f" ] || { echo '[]' > "$T/empty.json"; f="$T/empty.json"; }
    jq --arg c "$c" '[.[] | select(.headSha == $c)]' "$f" > "$T/commit.view.json"
    f="$T/commit.view.json"
  else
    if [ -f "$T/listing_error" ]; then cat "$T/listing_error" >&2; exit 1; fi
    f="$(snap listing)"
  fi
  if [ -n "$q" ]; then jq -r "$q" "$f"; else cat "$f"; fi
  exit 0
fi
if [ "$1 $2" = "run download" ]; then echo "fake gh: a resolver must never download" >&2; exit 1; fi
echo "fake gh: unexpected $*" >&2; exit 1
'''.replace("__T__", tmp)
        _write_exec(self.path, body)
        if head_error:
            with open(os.path.join(tmp, "head_error"), "w", encoding="utf-8") as f:
                f.write(head_error + "\n")
        if commit_error:
            with open(os.path.join(tmp, "commit_error"), "w", encoding="utf-8") as f:
                f.write(commit_error + "\n")
        if listing_error:
            with open(os.path.join(tmp, "listing_error"), "w", encoding="utf-8") as f:
                f.write(listing_error + "\n")
        self.sleep = os.path.join(tmp, "fake-sleep")
        _write_exec(self.sleep, '#!/usr/bin/env bash\nprintf "SLEEP %s\\n" "$*" >> "' + tmp + '/sleep.log"\n')

    def calls(self):
        return _read(self.log) if os.path.exists(self.log) else ""

    def sleeps(self):
        p = os.path.join(self.tmp, "sleep.log")
        return _read(p).splitlines() if os.path.exists(p) else []

    def count(self, kind):
        p = os.path.join(self.tmp, kind + ".count")
        return int(_read(p).strip()) if os.path.exists(p) else 0


def _resolve(fake, art=ART, extra_env=None, snippet=None):
    env = dict(os.environ)
    env.update({"CI_RUN_RESOLVE_GH": fake.path, "CI_RUN_RESOLVE_SLEEP": fake.sleep,
                "CI_RUN_RESOLVE_RETRIES": "3", "CI_RUN_RESOLVE_RETRY_S": "7"})
    if extra_env:
        env.update(extra_env)
    snippet = snippet or "ci_run_latest_success %s main ci.yml %s" % (GH_REPO, art)
    return subprocess.run(["bash", "-c", '. "%s"\n%s' % (RESOLVE_LIB, snippet)],
                          capture_output=True, text=True, env=env)


# =============================================================================================
# The live case: a STALE branch listing, the head's own run is found and picked
# =============================================================================================
def test_head_success_is_picked_even_when_the_branch_listing_is_stale():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_RUN, STALE_15_9, STALE_4_9]], LIVE_ARTS)
        r = _resolve(fake)
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == "36687519583", \
            "the stale listing walks to the 4.9. run 33857572305; the head's own run must win:\n" + r.stdout + r.stderr
        line = [ln for ln in r.stderr.splitlines() if "head-anchored" in ln]
        assert len(line) == 1, r.stderr
        assert "36687519583" in line[0] and "2026-09-30T08:05:08Z" in line[0] and "1f7e6569b" in line[0], line
        calls = fake.calls()
        assert "api repos/%s/branches/main --jq .commit.sha" % GH_REPO in calls, calls
        assert re.search(r"run list --repo %s --commit %s --workflow ci\.yml" % (GH_REPO, HEAD), calls), calls
        assert "--status" not in calls, "never a server-side status filter:\n" + calls
        assert "33857572305" not in r.stdout


def test_head_run_seen_only_in_the_branch_listing_is_still_head_anchored():
    # The head lookup lags but the branch listing already has the head's successful run.
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [[HEAD_RUN, STALE_15_9, STALE_4_9]], [[]], LIVE_ARTS)
        r = _resolve(fake)
        assert r.returncode == 0 and r.stdout.strip() == "36687519583", r.stdout + r.stderr
        assert "head-anchored" in r.stderr, r.stderr
        assert fake.sleeps() == [], "a head run in the listing is not a stale listing"


# =============================================================================================
# A STALE listing: no run for the head anywhere -> bounded re-read, then fail loud
# =============================================================================================
def test_stale_listing_without_the_head_is_retried_then_fails_loud():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[STALE_15_9, STALE_4_9]], LIVE_ARTS)
        r = _resolve(fake)
        assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
        assert r.stdout.strip() == "", "a stale listing must never pick an older run: %r" % r.stdout
        err = r.stderr
        assert HEAD in err and "stale GitHub listing" in err and "pass --run <id>" in err, err
        # read once + CI_RUN_RESOLVE_RETRIES (3) re-reads of BOTH, a CI_RUN_RESOLVE_RETRY_S sleep between
        assert fake.count("listing") == 4 and fake.count("commit") == 4, fake.calls()
        assert fake.sleeps() == ["SLEEP 7"] * 3, fake.sleeps()
        assert "artifacts" not in fake.calls(), "no older run may even be considered:\n" + fake.calls()


def test_stale_listing_that_catches_up_on_a_re_read_picks_the_head():
    with tempfile.TemporaryDirectory() as tmp:
        fresh = [HEAD_RUN, STALE_15_9, STALE_4_9]
        fake = FakeGh(tmp, HEAD, [STALE_LISTING, STALE_LISTING, fresh], [[], [], fresh], LIVE_ARTS)
        r = _resolve(fake)
        assert r.returncode == 0 and r.stdout.strip() == "36687519583", r.stdout + r.stderr
        assert fake.sleeps() == ["SLEEP 7"] * 2, fake.sleeps()
        assert "head-anchored" in r.stderr


def test_zero_retries_reads_once_and_never_sleeps():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[]], LIVE_ARTS)
        r = _resolve(fake, extra_env={"CI_RUN_RESOLVE_RETRIES": "0"})
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert fake.count("listing") == 1 and fake.sleeps() == [], fake.calls()
        assert "stale GitHub listing" in r.stderr


# =============================================================================================
# The head's run exists but is not usable -> the old walk, with ONE loud named line
# =============================================================================================
def _fallback_case(head_run, head_arts):
    with tempfile.TemporaryDirectory() as tmp:
        listing = [head_run, OLDER]
        arts = {800: [ART]}
        arts.update(head_arts)
        fake = FakeGh(tmp, HEAD_B, [listing], [listing], arts)
        r = _resolve(fake)
        return r, fake.sleeps()


def _assert_fallback(r, sleeps, state_words):
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "800", "the head's run is not usable -> the newest older success:\n" + r.stdout + r.stderr
    loud = [ln for ln in r.stderr.splitlines() if "FALLING BACK" in ln]
    assert len(loud) == 1, "exactly ONE loud fallback line:\n" + r.stderr
    ln = loud[0]
    assert "bbbbbbbbb" in ln, "the line names the head: " + ln
    for w in state_words:
        assert w in ln, "the line names the head's state (%s): %s" % (w, ln)
    assert "800" in ln and "2026-09-29T10:00:00Z" in ln and "aaaaaaaaa" in ln, \
        "the line names the fallback run id, date and sha: " + ln
    assert sleeps == [], "a head that HAS a run is not a stale listing"


def test_head_in_progress_falls_back_with_a_named_line():
    run = {"databaseId": 900, "createdAt": "2026-09-30T11:00:00Z", "status": "in_progress",
           "conclusion": "", "headSha": HEAD_B}
    r, s = _fallback_case(run, {})
    _assert_fallback(r, s, ["900", "in_progress"])


def test_head_queued_falls_back_with_a_named_line():
    run = {"databaseId": 901, "createdAt": "2026-09-30T11:00:00Z", "status": "queued",
           "conclusion": "", "headSha": HEAD_B}
    r, s = _fallback_case(run, {})
    _assert_fallback(r, s, ["901", "queued"])


def test_head_failed_falls_back_with_a_named_line():
    run = {"databaseId": 902, "createdAt": "2026-09-30T11:00:00Z", "status": "completed",
           "conclusion": "failure", "headSha": HEAD_B}
    r, s = _fallback_case(run, {})
    _assert_fallback(r, s, ["902", "failure"])


def test_head_success_with_an_expired_artifact_falls_back_with_a_named_line():
    run = {"databaseId": 903, "createdAt": "2026-09-30T11:00:00Z", "status": "completed",
           "conclusion": "success", "headSha": HEAD_B}
    r, s = _fallback_case(run, {903: [(ART, True)]})
    _assert_fallback(r, s, ["903", "artifact"])


# =============================================================================================
# The fallback walks ONLY a listing that holds the head: a listing without the head's run is stale
# even when the head lookup already shows that run in flight (review round 1)
# =============================================================================================
def test_fallback_never_walks_a_stale_listing_while_the_head_run_is_in_flight():
    # The 30.9. case one step earlier: the head's CI is still running and the listing is the stale
    # set. Walking it would deploy the 4.9. run again.
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_INPROG]], LIVE_ARTS)
        r = _resolve(fake)
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert HEAD in r.stderr and "stale GitHub listing" in r.stderr and "pass --run <id>" in r.stderr, r.stderr
        assert "runs/33857572305/artifacts" not in fake.calls(), "the stale listing was walked:\n" + fake.calls()
        assert fake.count("listing") == 4 and fake.sleeps() == ["SLEEP 7"] * 3, fake.calls()


def test_a_listing_proven_stale_before_the_head_run_appears_is_never_walked():
    # Read 1: no head run anywhere. Read 2: the head lookup now shows the head's run in flight, but
    # the listing is still the stale set -- it is re-read, never walked from read 1.
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[], [HEAD_INPROG]], LIVE_ARTS)
        r = _resolve(fake)
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert "stale GitHub listing" in r.stderr, r.stderr
        assert "runs/33857572305/artifacts" not in fake.calls(), fake.calls()


def test_a_stale_listing_that_catches_up_while_the_head_is_in_flight_falls_back():
    newer = {"databaseId": 36600000000, "createdAt": "2026-09-29T12:00:00Z", "status": "completed",
             "conclusion": "success", "headSha": "c" * 40}
    fresh = [HEAD_INPROG, newer, STALE_15_9, STALE_4_9]
    arts = dict(LIVE_ARTS)
    arts[36600000000] = [ART]
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING, fresh], [[HEAD_INPROG]], arts)
        r = _resolve(fake)
        assert r.returncode == 0 and r.stdout.strip() == "36600000000", r.stdout + r.stderr
        assert fake.sleeps() == ["SLEEP 7"], fake.sleeps()
        loud = [ln for ln in r.stderr.splitlines() if "FALLING BACK" in ln]
        assert len(loud) == 1 and "in_progress" in loud[0] and "36687519583" in loud[0], r.stderr
        assert "runs/33857572305/artifacts" not in fake.calls(), fake.calls()


def test_a_leading_zero_retry_count_is_read_as_decimal():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[]], LIVE_ARTS)
        r = _resolve(fake, extra_env={"CI_RUN_RESOLVE_RETRIES": "08"})
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert "value too great" not in r.stderr, r.stderr
        assert "stale GitHub listing" in r.stderr and fake.sleeps() == ["SLEEP 7"] * 8, r.stderr


# =============================================================================================
# gh errors on the head side fail loud -- never a fallback
# =============================================================================================
def test_gh_error_reading_the_head_fails_loud():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_RUN]], LIVE_ARTS,
                      head_error="HTTP 502: Bad Gateway (https://api.github.com/repos/x/branches/main)")
        r = _resolve(fake)
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert "HTTP 502: Bad Gateway" in r.stderr and "pass --run <id>" in r.stderr, r.stderr
        assert "run list" not in fake.calls(), "no listing may be read without the head:\n" + fake.calls()


def test_a_head_that_is_not_a_commit_sha_fails_loud():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, "null", [STALE_LISTING], [[HEAD_RUN]], LIVE_ARTS)
        r = _resolve(fake)
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert "not a commit sha" in r.stderr, r.stderr
        assert "run list" not in fake.calls(), fake.calls()


def test_gh_error_on_the_head_run_lookup_fails_loud():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_RUN]], LIVE_ARTS, commit_error="HTTP 500: boom")
        r = _resolve(fake)
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert "HTTP 500: boom" in r.stderr, r.stderr
        assert "artifacts" not in fake.calls(), "never a fallback walk:\n" + fake.calls()


def test_a_listing_error_never_refuses_a_known_good_head_run():
    # The head lookup already has the head's successful run carrying the artifact: the branch
    # listing is not needed, so a listing hiccup must not turn a known-good pick into a refusal.
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_RUN]], LIVE_ARTS,
                      listing_error="HTTP 502: Bad Gateway (listing)")
        r = _resolve(fake)
        assert r.returncode == 0 and r.stdout.strip() == "36687519583", r.stdout + r.stderr
        assert "head-anchored" in r.stderr, r.stderr
        assert "--branch main" not in fake.calls(), "the listing is read only when needed:\n" + fake.calls()


def test_a_listing_error_on_the_fallback_path_fails_loud():
    # The head's run is in flight, so the fallback needs the listing; a gh error there is a loud
    # refusal, never a guess.
    with tempfile.TemporaryDirectory() as tmp:
        run = {"databaseId": 900, "createdAt": "2026-09-30T11:00:00Z", "status": "in_progress",
               "conclusion": "", "headSha": HEAD_B}
        fake = FakeGh(tmp, HEAD_B, [[run, OLDER]], [[run, OLDER]], {800: [ART]},
                      listing_error="HTTP 502: Bad Gateway (listing)")
        r = _resolve(fake)
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert "HTTP 502: Bad Gateway (listing)" in r.stderr and "pass --run <id>" in r.stderr, r.stderr


def test_an_unreadable_head_artifact_list_stops_the_resolver():
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_RUN]], LIVE_ARTS)
        body = _read(fake.path).replace(
            "    */artifacts*)\n",
            '    */artifacts*)\n      case "$2" in *runs/36687519583/*) echo "HTTP 502: Bad Gateway" >&2; exit 1 ;; esac\n', 1)
        _write_exec(fake.path, body)
        r = _resolve(fake)
        assert r.returncode == 1 and r.stdout.strip() == "", r.stdout + r.stderr
        assert "UNREADABLE" in r.stderr, r.stderr


# =============================================================================================
# setup-device.sh uses the ONE lib for both of its default lookups
# =============================================================================================
CALL_RE = re.compile(
    r'^\s*(RUN_ID|PROBE_RUN_ID)="\$\(ci_run_latest_success "\$GITHUB_REPO" "\$CI_BRANCH" ci\.yml ([a-z0-9-]+)\)"(.*)$')


def _setup_calls():
    return [(m.group(1), m.group(2), m.group(0).strip())
            for m in (CALL_RE.match(ln) for ln in _noncomment(_read(SETUP)).splitlines()) if m]


def test_setup_device_sources_the_lib_and_has_no_inline_run_query():
    body = _read(SETUP)
    assert '. "$HERE/lib/ci-run-resolve.sh"' in _noncomment(body), "setup-device must source the resolver lib"
    code = _noncomment(body)
    assert "gh run list" not in code, "no inline run listing left in setup-device.sh"
    assert "--status success" not in code, "no server-side status filter left in setup-device.sh"
    assert ".[0].databaseId" not in code
    # the explicit --run paths stay as they were
    assert 'RUN_ID="$CI_RUN_ID_ARG"' in code and 'PROBE_RUN_ID="$CI_RUN_ID_ARG"' in code


def test_setup_device_both_lookups_call_the_lib():
    calls = _setup_calls()
    assert sorted((v, a) for v, a, _ in calls) == [
        ("PROBE_RUN_ID", "probe-tools-linux-amd64"),
        ("RUN_ID", "camera-box-linux-amd64"),
    ], calls


def test_setup_device_lookup_lines_resolve_the_head_through_a_fake_gh():
    # Run each of setup-device's own call lines (verbatim) with the lib sourced and a fake gh on
    # PATH: the stale listing must not win, the head's run must.
    calls = _setup_calls()
    assert len(calls) == 2, "both setup-device lookups must be lib calls: %r" % calls
    for var, art, line in calls:
        with tempfile.TemporaryDirectory() as tmp:
            fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_RUN, STALE_15_9, STALE_4_9]],
                          {36687519583: [art], 33857572305: [art]})
            env = dict(os.environ)
            env.pop("CI_RUN_RESOLVE_GH", None)
            env.update({"PATH": tmp + os.pathsep + env.get("PATH", ""),
                        "CI_RUN_RESOLVE_SLEEP": fake.sleep})
            snippet = ('. "%s"\nGITHUB_REPO=%s\nCI_BRANCH=main\n%s\nprintf "%%s" "$%s"\n'
                       % (RESOLVE_LIB, GH_REPO, line, var))
            r = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, env=env)
            assert r.returncode == 0, r.stderr
            assert r.stdout == "36687519583", (var, art, r.stdout, r.stderr)


# =============================================================================================
# bkshading-deploy-service.sh (the strih service deploy) uses the same lib (review round 1)
# =============================================================================================
SERVICE_CALL_RE = re.compile(
    r'^\s*RUN_ID="\$\(CI_RUN_RESOLVE_GH="\$GH" ci_run_latest_success "\$REPO" "\$BRANCH" ci\.yml "\$ARTIFACT"\)"'
    r' \|\| RUN_ID=""\s*$')


def test_deploy_service_resolves_its_run_through_the_lib():
    code = _noncomment(_read(DEPLOY_SERVICE))
    assert '. "$HERE/lib/ci-run-resolve.sh"' in code, "bkshading-deploy-service.sh must source the resolver lib"
    assert "gh run list" not in code and '"$GH" run list' not in code, "no inline run listing left"
    assert "--status success" not in code
    lines = [ln for ln in code.splitlines() if SERVICE_CALL_RE.match(ln)]
    assert len(lines) == 1, lines
    with tempfile.TemporaryDirectory() as tmp:
        fake = FakeGh(tmp, HEAD, [STALE_LISTING], [[HEAD_RUN]], {36687519583: ["bkshading-windows-amd64"]})
        env = dict(os.environ)
        env.pop("CI_RUN_RESOLVE_GH", None)
        env["CI_RUN_RESOLVE_SLEEP"] = fake.sleep
        snippet = ('. "%s"\nGH=%s\nREPO=%s\nBRANCH=main\nARTIFACT=bkshading-windows-amd64\n%s\n'
                   'printf "%%s" "$RUN_ID"\n' % (RESOLVE_LIB, fake.path, GH_REPO, lines[0].strip()))
        r = subprocess.run(["bash", "-c", snippet], capture_output=True, text=True, env=env)
        assert r.returncode == 0 and r.stdout == "36687519583", (r.stdout, r.stderr)
