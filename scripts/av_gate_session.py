#!/usr/bin/env python3
"""Issue 1404 -- the camera-box client of restreamer's YouTube A/V-gate session API.

restreamer owns the YouTube leg's delivery: `/api/v1/av-gate/session` (restreamer
`crates/rs-api/src/av_gate*.rs`, its rule `.claude/rules/youtube-av-measurement.md`) creates an
unlisted broadcast bound to the CI test stream "e2e rtmp", starts the E2E-Test event's delivery VPS
and, after a stop, drains, completes the broadcast, deletes the VPS and waits for the VOD. The
caller drives OBS (StartStream / StopStream) and the measurement. camera-box never touches
restreamer's VPS, keys or YouTube account itself.

The API listens on the stream box and accepts only a LAN origin plus `Authorization: Bearer <the
content of av_gate.api_token_file>`. The token never leaves the stream box: every request runs ON
the box over ssh, curl reads the header from its stdin (`curl -K -`), which a small shell pipeline
fills from the token file in place. The token is never in an argv, an environment variable on dev1,
a log line or this script's output.

  av_gate_session.py --host H --user U start --requester R [--title T]
  av_gate_session.py --host H --user U status --id ID
  av_gate_session.py --host H --user U wait-ready --id ID [--deadline S]
  av_gate_session.py --host H --user U stop --id ID
  av_gate_session.py --host H --user U wait-done --id ID [--deadline S]

stdout = ONE JSON object: `{"ok": true, ...}` or `{"ok": false, "reason": "..."}`.
exit 0 = OK, 2 = UNKNOWN (every refusal, failure, unreachable API and timeout; fail closed).
The ssh password comes from $AV_GATE_SSH_PASSWORD (sshpass -e), never an argument.
Options: --api (default http://127.0.0.1:8910, as the box sees it), --token-file (default
~/.config/restreamer/av-gate/api-token, restreamer's Linux default), --poll (5 s).
"""
import argparse
import json
import os
import shlex
import subprocess
import sys
import time

EXIT_OK, EXIT_UNKNOWN = 0, 2
DEFAULT_API = "http://127.0.0.1:8910"
DEFAULT_TOKEN_FILE = "~/.config/restreamer/av-gate/api-token"
SESSION_PATH = "/api/v1/av-gate/session"
SSH_OPTS = ["-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=10"]
REQUEST_TIMEOUT_S = 60       # one request, ssh included
CURL_MAX_TIME_S = 30
MAX_POLL_ERRORS = 3          # consecutive failed status reads before a wait gives up
HTTP_MARK, NO_TOKEN_MARK, CURL_RC_MARK = "AVGATE-HTTP ", "AVGATE-NO-TOKEN", "AVGATE-CURL-RC "


class Unknown(Exception):
    """A refusal, failure or timeout: the caller's stage is UNKNOWN (red)."""


def _remote_path(path):
    """A token path for the box's shell: `~/x` -> "$HOME"/'x', anything else single-quoted."""
    if path.startswith("~/"):
        return '"$HOME"/' + shlex.quote(path[2:])
    return shlex.quote(path)


def remote_script(method, url, body, token_file):
    """The bash text run ON the stream box for one request. curl's config (the Authorization
    header) arrives on its stdin from the token file; quotes and backslashes in the token are
    escaped for curl's config syntax."""
    data = f" --data-binary {shlex.quote(body)}" if body is not None else ""
    return (
        f"tok={_remote_path(token_file)}\n"
        f'if [ ! -r "$tok" ]; then echo {NO_TOKEN_MARK}; exit 0; fi\n'
        "{ printf 'header = \"Authorization: Bearer '; tr -d '\\r\\n' < \"$tok\" | sed 's/[\\\\\"]/\\\\&/g';"
        " printf '\"\\n'; } | "
        f"curl -sS --max-time {CURL_MAX_TIME_S} -K - -X {method} -H 'Content-Type: application/json'{data}"
        f" -w '\\n{HTTP_MARK}%{{http_code}}\\n' {shlex.quote(url)}\n"
        f'echo "{CURL_RC_MARK}$?"\n'
    )


def parse_response(text, host, token_file):
    """(status code, decoded JSON body | None) from the remote script's output; Unknown when the
    token file is missing or curl never reached the API."""
    if NO_TOKEN_MARK in text.splitlines():
        raise Unknown(f"the av-gate API token {token_file} is missing on {host}: restreamer has not "
                      "provisioned the YouTube A/V-gate session API there (its av-gate dir: api-token + "
                      "the manage-scope oauth file)")
    head, sep, tail = text.rpartition("\n" + HTTP_MARK)
    if not sep:
        raise Unknown(f"no HTTP status from the av-gate API on {host}: {text.strip()[-300:] or '<no output>'}")
    code = tail.split()[0] if tail.split() else ""
    if not code.isdigit() or code == "000":
        raise Unknown(f"the restreamer API on {host} is unreachable (curl status {code or '?'}): "
                      f"{head.strip()[-300:]}")
    try:
        body = json.loads(head) if head.strip() else None
    except ValueError:
        body = None
    return int(code), body


class Client:
    def __init__(self, host, user, api=DEFAULT_API, token_file=DEFAULT_TOKEN_FILE, poll_s=5.0,
                 clock=time.monotonic, sleep=time.sleep):
        self.host, self.user, self.api, self.token_file = host, user, api.rstrip("/"), token_file
        self.poll_s, self.clock, self.sleep = poll_s, clock, sleep

    def request(self, method, path, body=None):
        script = remote_script(method, self.api + path, None if body is None else json.dumps(body),
                               self.token_file)
        cmd = ["sshpass", "-e", "ssh", "-n", *SSH_OPTS, f"{self.user}@{self.host}", script]
        env = dict(os.environ, SSHPASS=os.environ.get("AV_GATE_SSH_PASSWORD", ""))
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=REQUEST_TIMEOUT_S, env=env)
        except subprocess.TimeoutExpired:
            raise Unknown(f"the request to the av-gate API on {self.host} took over {REQUEST_TIMEOUT_S} s")
        except OSError as e:
            raise Unknown(f"cannot run sshpass/ssh: {e}")
        if r.returncode != 0 and HTTP_MARK not in r.stdout:
            raise Unknown(f"ssh to {self.user}@{self.host} failed (rc {r.returncode}): "
                          f"{(r.stderr or r.stdout).strip()[-300:]}")
        return parse_response(r.stdout, self.host, self.token_file)

    # ---------------------------------------------------------------- the API calls

    def start(self, requester, title=None):
        body = {"requester": requester}
        if title:
            body["title"] = title
        code, j = self.request("POST", SESSION_PATH, body)
        j = j if isinstance(j, dict) else {}
        if code == 201 and j.get("session_id"):
            return {"session_id": j["session_id"], "broadcast_id": j.get("broadcast_id")}
        raise Unknown("restreamer refused the YouTube session: " + refusal_text(code, j))

    def status(self, sid):
        code, j = self.request("GET", f"{SESSION_PATH}/{sid}")
        if code == 200 and isinstance(j, dict):
            return j
        raise Unknown(f"session {sid} status read failed: " + refusal_text(code, j if isinstance(j, dict) else {}))

    def stop(self, sid):
        code, j = self.request("POST", f"{SESSION_PATH}/{sid}/stop")
        if code in (200, 202):
            return {"stopped": True, "http": code}
        raise Unknown(f"session {sid} stop refused: " + refusal_text(code, j if isinstance(j, dict) else {}))

    def _wait(self, sid, want, deadline_s, what):
        end, errors, last = self.clock() + deadline_s, 0, None
        while True:
            try:
                st = self.status(sid)
                errors = 0
            except Unknown as e:
                errors += 1
                if errors >= MAX_POLL_ERRORS:
                    raise Unknown(f"{what}: {MAX_POLL_ERRORS} status reads in a row failed, last: {e}")
                st = None
            if st is not None:
                last = st.get("state")
                if last == want:
                    return st
                if last == "failed":
                    raise Unknown(f"restreamer failed the session {sid}: {st.get('reason') or 'no reason given'}")
                if want == "ready" and last in ("processing", "done"):
                    raise Unknown(f"session {sid} is already {last} (stopped before it was ready)")
            if self.clock() >= end:
                raise Unknown(f"{what} within {deadline_s:.0f} s (last state {last or 'unread'})")
            self.sleep(self.poll_s)

    def wait_ready(self, sid, deadline_s=420.0):
        self._wait(sid, "ready", deadline_s, "the YouTube broadcast did not go live")
        return {"state": "ready"}

    def wait_done(self, sid, deadline_s=1800.0):
        st = self._wait(sid, "done", deadline_s, "the YouTube VOD was not processed")
        vod = st.get("vod_id") or st.get("broadcast_id")
        if not vod:
            raise Unknown(f"session {sid} is done but names no vod_id")
        return {"state": "done", "vod_id": vod}


def refusal_text(code, j):
    """One readable line for a non-success answer of the API."""
    err, detail = j.get("error"), j.get("detail") or j.get("reason")
    if code == 409 and err == "busy":
        h = j.get("holder") or {}
        return (f"HTTP 409 busy: another av-gate session holds the rig "
                f"({h.get('requester') or '?'} session {h.get('session_id') or '?'})")
    if code == 409 and err == "cleanup_pending":
        return f"HTTP 409 cleanup_pending: an earlier teardown is still being retried ({j.get('sessions')})"
    if code == 429:
        return f"HTTP 429 {err or 'quota'}: the YouTube API quota budget is spent"
    if code == 503 and err == "not_provisioned":
        return f"HTTP 503 not_provisioned: {detail or 'no usable av-gate token or oauth file on the box'}"
    if code == 502:
        return f"HTTP 502 the session start failed and was torn down: {detail or '?'}"
    return f"HTTP {code} {err or ''} {detail or ''}".strip()


def main(argv=None):
    ap = argparse.ArgumentParser(description="issue 1404: restreamer av-gate session client (exit 0 OK, 2 UNKNOWN)")
    ap.add_argument("--host", required=True)
    ap.add_argument("--user", required=True)
    ap.add_argument("--api", default=DEFAULT_API)
    ap.add_argument("--token-file", default=DEFAULT_TOKEN_FILE)
    ap.add_argument("--poll", type=float, default=5.0)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("start")
    p.add_argument("--requester", required=True)
    p.add_argument("--title")
    for name in ("status", "stop", "wait-ready", "wait-done"):
        p = sub.add_parser(name)
        p.add_argument("--id", required=True)
        if name.startswith("wait-"):
            p.add_argument("--deadline", type=float, default=420.0 if name == "wait-ready" else 1800.0)
    a = ap.parse_args(argv)
    c = Client(a.host, a.user, a.api, a.token_file, a.poll)
    try:
        if a.cmd == "start":
            out = c.start(a.requester, a.title)
        elif a.cmd == "status":
            out = c.status(a.id)
        elif a.cmd == "stop":
            out = c.stop(a.id)
        elif a.cmd == "wait-ready":
            out = c.wait_ready(a.id, a.deadline)
        else:
            out = c.wait_done(a.id, a.deadline)
    except Unknown as e:
        print(json.dumps({"ok": False, "reason": str(e)}))
        return EXIT_UNKNOWN
    print(json.dumps(dict(out, ok=True)))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
