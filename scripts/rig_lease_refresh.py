#!/usr/bin/env python3
"""issue 1383 -- the rig-lease HOLDER's own keep-alive: one check-and-update of the #830 lockdir.

WHY: the dev1 rig lease (`/var/tmp/rig-lease/holder.json` + `heartbeat`, scripts/lib/rig-lease.sh)
was touched only at acquire and declared `expected_release_at` = acquire + 45 min, so a live
~60-70 min release E2E read as "hung" on the :8890 JSON (27.9.2026 22:15 UTC). The holder now
beats its own lease while alive: scripts/lib/rig-lease.sh `rig_lease_refresh_if_mine` (a thin
wrapper over this CLI) is called by the E2E/soak heartbeat refresher and by the gate's
lease-only keep-alive.

One refresh, ONLY when holder.json names exactly (repo, run_id):
  - past acquired_at + max_hold_s (the hold CEILING, default = the full-path job timeout) nothing
    is written: a stuck-but-alive holder must still age into the heartbeat-stale reclaim;
  - bump the heartbeat mtime;
  - roll expected_release_at to max(current, now + lookahead_s) -- never backward.

Safety (every file operation goes through a directory FD of the lease dir):
  - a missing lease dir, a missing/corrupt holder.json, a foreign holder or an empty identity is a
    no-op that creates nothing;
  - holder.json is replaced by an O_EXCL temp + rename, never rewritten in place, so a concurrent
    reader sees the old or the new complete file;
  - just before the rename holder.json is re-read (a concurrent reclaim rewrote it -> it wins) and
    the lease PATH is checked to still be the directory we opened (a concurrent release renamed it
    aside, maybe a peer already mkdir'ed a new one -> nothing is renamed); what remains is the
    microsecond window between those checks and the rename, unreachable while the holder beats
    (a reclaim needs a heartbeat stale for RIG_LEASE_STALE_SECS);
  - the heartbeat and the temp are opened O_NOFOLLOW.

Exit codes / status line (`RIG_LEASE_REFRESH=<status>`, exactly one line on stdout):
  0 refreshed expected_release_at=<ts>
  1 not-mine holder=<repo>#<run_id> | no-lease | no-holder | corrupt | no-identity
    | released-during-refresh
  2 error <what>          (an unexpected filesystem error -- the lease may still be ours)
  3 over-hold ...         (ours, but past the hold ceiling or with no parseable acquired_at)

CLI: rig_lease_refresh.py <lease_dir> <repo> <run_id> <lookahead_s> <max_hold_s>
"""
from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timedelta, timezone
from typing import Callable, Optional

ISO = "%Y-%m-%dT%H:%M:%SZ"
DEFAULT_LOOKAHEAD_S = 900
DEFAULT_MAX_HOLD_S = 4500

RC_REFRESHED = 0
RC_NOT_OURS = 1
RC_ERROR = 2
RC_OVER_HOLD = 3

_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)


def parse_iso(ts) -> Optional[datetime]:
    try:
        return datetime.strptime(str(ts), ISO).replace(tzinfo=timezone.utc)
    except ValueError:
        return None


def _read_holder(dfd: int):
    """-> (dict, "") or (None, "no-holder" | "corrupt")."""
    try:
        fd = os.open("holder.json", os.O_RDONLY, dir_fd=dfd)
    except FileNotFoundError:
        return None, "no-holder"
    with os.fdopen(fd) as f:
        try:
            data = json.load(f)
        except ValueError:
            return None, "corrupt"
    if not isinstance(data, dict):
        return None, "corrupt"
    return data, ""


def _owner(data: dict):
    return str(data.get("repo", "") or ""), str(data.get("run_id", "") or "")


def _same_dir(lease_dir: str, dfd: int) -> bool:
    """-> True while the lease PATH still names the directory the FD was opened on."""
    try:
        st = os.stat(lease_dir)
    except FileNotFoundError:
        return False
    here = os.fstat(dfd)
    return (st.st_dev, st.st_ino) == (here.st_dev, here.st_ino)


def _unlink_if_present(name: str, dfd: int) -> bool:
    """-> True when a file was removed; an absent file is the normal case (False)."""
    try:
        os.unlink(name, dir_fd=dfd)
    except FileNotFoundError:
        return False
    return True


def _refresh_in(dfd: int, lease_dir: str, repo: str, run_id: str, lookahead_s: int,
                max_hold_s: int, now: datetime,
                before_rename: Optional[Callable[[], None]]):
    data, why = _read_holder(dfd)
    if data is None:
        return RC_NOT_OURS, why
    if _owner(data) != (repo, run_id):
        return RC_NOT_OURS, "not-mine holder=%s#%s" % _owner(data)

    acquired = parse_iso(data.get("acquired_at", ""))
    if acquired is None:
        return RC_OVER_HOLD, "over-hold acquired_at=unparseable"
    if now > acquired + timedelta(seconds=max_hold_s):
        return RC_OVER_HOLD, "over-hold acquired_at=%s max_hold_s=%d" % (
            data.get("acquired_at"), max_hold_s)

    hb = os.open("heartbeat", os.O_WRONLY | os.O_CREAT | _NOFOLLOW, 0o666, dir_fd=dfd)
    try:
        os.utime(hb)
    finally:
        os.close(hb)

    target = now + timedelta(seconds=lookahead_s)
    current = parse_iso(data.get("expected_release_at", ""))
    if current is not None and current >= target:
        return RC_REFRESHED, "refreshed expected_release_at=%s" % data["expected_release_at"]

    data["expected_release_at"] = target.strftime(ISO)
    tmp = "holder.json.refresh.%d" % os.getpid()
    _unlink_if_present(tmp, dfd)  # a leftover of a refresh killed mid-write under a recycled pid
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | _NOFOLLOW, 0o666, dir_fd=dfd)
    with os.fdopen(fd, "w") as f:
        json.dump(data, f)
    if before_rename is not None:
        before_rename()  # test seam: where a concurrent release/reclaim lands
    again, _ = _read_holder(dfd)
    if again is None or _owner(again) != (repo, run_id):
        _unlink_if_present(tmp, dfd)
        return RC_NOT_OURS, "not-mine holder=changed-before-the-rename"
    if not _same_dir(lease_dir, dfd):
        _unlink_if_present(tmp, dfd)
        return RC_NOT_OURS, "released-during-refresh"
    os.replace(tmp, "holder.json", src_dir_fd=dfd, dst_dir_fd=dfd)
    return RC_REFRESHED, "refreshed expected_release_at=%s" % data["expected_release_at"]


def refresh(lease_dir: str, repo: str, run_id: str, lookahead_s: int = DEFAULT_LOOKAHEAD_S,
            max_hold_s: int = DEFAULT_MAX_HOLD_S, *, now: Optional[datetime] = None,
            before_rename: Optional[Callable[[], None]] = None):
    """One keep-alive beat -> (exit code, status). See the module doc."""
    if not repo or not run_id:
        return RC_NOT_OURS, "no-identity"
    if now is None:
        now = datetime.now(timezone.utc).replace(microsecond=0)
    try:
        dfd = os.open(lease_dir, os.O_RDONLY | os.O_DIRECTORY)
    except (FileNotFoundError, NotADirectoryError):
        return RC_NOT_OURS, "no-lease"
    except OSError as e:
        return RC_ERROR, "error %s" % e
    try:
        return _refresh_in(dfd, lease_dir, repo, run_id, lookahead_s, max_hold_s, now,
                           before_rename)
    except OSError as e:
        return RC_ERROR, "error %s" % e
    finally:
        os.close(dfd)


def _int_or(value: str, default: int) -> int:
    return int(value) if value.isdigit() else default


def main(argv) -> int:
    if len(argv) != 6:
        print("usage: rig_lease_refresh.py <lease_dir> <repo> <run_id> <lookahead_s> <max_hold_s>",
              file=sys.stderr)
        return RC_ERROR
    _, lease_dir, repo, run_id, lookahead, max_hold = argv
    rc, status = refresh(lease_dir, repo, run_id, _int_or(lookahead, DEFAULT_LOOKAHEAD_S),
                         _int_or(max_hold, DEFAULT_MAX_HOLD_S))
    print("RIG_LEASE_REFRESH=" + status)
    return rc


if __name__ == "__main__":
    sys.exit(main(sys.argv))
