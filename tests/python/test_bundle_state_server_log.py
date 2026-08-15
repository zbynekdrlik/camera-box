"""#829 — regression test for scripts/bundle-state-server.py's log() surviving a DEAD stdout pipe.

Live incident (stream box, 2026-08-15): a hidden Scheduled-Task context handed the process a dead
stdout pipe, so `print(..., flush=True)` raised `OSError [Errno 22]` INSIDE the request handler,
killing every request before it served ("connection closed unexpectedly" with zero log lines).
Logging must never take the server down. This pins that log() swallows a broken-stdout write.

Same "source parsers, verify live separately" split as test_bundle_state_gather.py — but
bundle-state-server.py is hyphenated (not a normal import name) and __main__-guarded, so it is loaded
by file path via importlib without starting the HTTP server.
"""
import importlib.util
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
# bundle-state-server.py does `import bundle_state_gather` at module scope, so scripts/ must be
# importable before we exec it.
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

_SPEC = importlib.util.spec_from_file_location(
    "bundle_state_server", _SCRIPTS / "bundle-state-server.py"
)
bss = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bss)  # __name__ != "__main__" -> main()/serve_forever() does NOT run


class _DeadStdout:
    """A stdout whose write/flush raise OSError(EINVAL) — the exact hidden-task dead-pipe shape."""

    def write(self, *_a, **_k):
        raise OSError(22, "Invalid argument")

    def flush(self, *_a, **_k):
        raise OSError(22, "Invalid argument")


def test_log_survives_a_dead_stdout_pipe(monkeypatch):
    # Under the pre-#829 log() (`print(..., flush=True)` with no guard) this raises OSError and
    # would kill the request handler; after the fix it must return normally.
    monkeypatch.setattr(sys, "stdout", _DeadStdout())
    bss.log("hidden-task heartbeat")  # must NOT raise


def test_log_writes_normally_to_a_live_stdout(capsys):
    # The happy path is unchanged: a live stdout still gets the timestamped line.
    bss.log("normal line")
    out = capsys.readouterr().out
    assert "normal line" in out
