#!/usr/bin/env python3
"""#802 -- SRT-tap safety: the launch-path guard + listener-mode redesign that keeps the
A/V-sync SRT tap from ever crashing OBS.

Background (LIVE crash, 2026-07-19, stream box): the A/V-sync monitor (#801) taps the stream
program as an SRT copy (srt://...:9998). Starting an Aitum Multistream SRT-**CALLER** output
against an unreachable listener crashed the whole OBS process -- the failed-start cleanup path in
vendored `obs-ffmpeg-mpegts.c` (`connect_mpegts_url` -> `libsrt_open` < 0 -> "Failed to open the
url" -> process died). VLC 3.x SRT is caller-only, so the intended local listener never bound.

The redesign (#802 plan #2): the OBS-side tap output is an SRT **LISTENER**
(`srt://0.0.0.0:9998?mode=listener`) -- a listener bind succeeds immediately with **no peer**, so
the failing-start path is never reached and the crash trigger STRUCTURALLY cannot occur. The
player (VLC/ffmpeg, both caller-capable) connects on demand as the CALLER.

camera-box cannot create the Aitum output over obs-websocket (no such request), so it cannot
physically stop a hand-configured caller output -- but it OWNS the canonical config + the guard +
the bench harness, making the safe path the easy path. This module is the single source of truth:

  * recommend the canonical listener tap URL              -> recommend_tap_url()
  * classify any SRT URL's mode / target                  -> srt_mode() / parse_srt_target()
  * REFUSE to start a caller/rendezvous output (the       -> assert_safe_to_start()
    crash-prone modes); a listener URL is always safe
  * READER side (a caller grabbing FROM the tap): fast    -> reader_should_grab()
    NO-SIGNAL when nothing is listening, never a crash

Pure stdlib (socket + urllib.parse) -- Tier-0, unit-tested under tests/python/test_srt_tap.py.

Sign/convention notes:
  * libsrt / ffmpeg SRT default mode is CALLER when no `mode=` query param is present.
  * A LISTENER bind never fails on "no peer" -- that is the whole point of the redesign.
"""

import argparse
import errno
import socket
import sys
from urllib.parse import parse_qs, urlsplit

SRT_TAP_DEFAULT_PORT = 9998
# libsrt / ffmpeg: absent `mode=` -> caller (the crash-prone mode for the tap).
DEFAULT_SRT_MODE = "caller"
# Modes that OPEN a connection to a peer at start -> can fail the start -> can crash OBS.
CRASH_PRONE_MODES = ("caller", "rendezvous")


class UnsafeTapError(Exception):
    """Raised by assert_safe_to_start() when an SRT URL would start in a crash-prone mode."""


def parse_srt_target(url):
    """`srt://HOST:PORT?...` -> (host, port). Raises ValueError on a non-srt URL or a missing
    port (an SRT endpoint without an explicit port is not a valid tap target)."""
    parts = urlsplit(url)
    if parts.scheme != "srt":
        raise ValueError(f"not an srt:// URL: {url!r}")
    host = parts.hostname
    port = parts.port
    if not host or port is None:
        raise ValueError(f"srt URL missing host or port: {url!r}")
    return host, port


def _srt_query(url):
    """Case-insensitive dict of the SRT query params (last value wins), keys lower-cased."""
    q = parse_qs(urlsplit(url).query, keep_blank_values=True)
    return {k.lower(): v[-1] for k, v in q.items()}


def srt_mode(url):
    """"listener" / "caller" / "rendezvous" -- from the `mode=` param, defaulting to caller
    (libsrt/ffmpeg convention). An unrecognised mode value is returned verbatim (lower-cased) so
    the caller can decide -- assert_safe_to_start() treats anything that is not "listener" as
    crash-prone, which is the safe default."""
    return _srt_query(url).get("mode", DEFAULT_SRT_MODE).strip().lower()


def is_listener_url(url):
    """True iff the SRT URL starts in LISTENER mode (bind-only, never fails on a missing peer)."""
    return srt_mode(url) == "listener"


def recommend_tap_url(port=SRT_TAP_DEFAULT_PORT, host="0.0.0.0", extra_params=None):
    """The canonical, crash-safe tap URL for the OBS-side output: an SRT LISTENER. Binding
    `0.0.0.0` accepts the player from localhost or the LAN. Extra libsrt params (e.g.
    `latency`, `pkt_size`) may be appended via `extra_params` (an ordered dict / list of
    (k, v) pairs)."""
    params = [("mode", "listener")]
    if extra_params:
        items = extra_params.items() if hasattr(extra_params, "items") else list(extra_params)
        params.extend((k, v) for k, v in items if k.lower() != "mode")
    query = "&".join(f"{k}={v}" for k, v in params)
    return f"srt://{host}:{port}?{query}"


def probe_listener_bindable(host, port, timeout=1.0):
    """True iff a UDP socket can BIND host:port right now -- the precondition for the OBS-side
    LISTENER tap to start. False if the port is already taken (EADDRINUSE) -- which, for the tap
    port, means a listener is already up (also fine). Returns None if the address is unusable for
    a reason other than in-use (e.g. host not local) -- the caller should treat None as
    'cannot confirm', never as 'safe'. Never raises."""
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    except OSError:
        return None
    try:
        s.settimeout(timeout)
        try:
            s.bind((host, port))
        except OSError as exc:
            # EADDRINUSE -> a listener already holds the port (fine); any other bind error
            # (e.g. cannot assign a non-local address) is 'cannot confirm'. errno.EADDRINUSE is
            # portable (98 on Linux, 48 on macOS, WSAEADDRINUSE 10048 on Windows).
            return False if exc.errno == errno.EADDRINUSE else None
        return True
    finally:
        s.close()


def probe_udp_port_refused(host, port, timeout=1.0):
    """Best-effort 'is there DEFINITELY nothing listening' probe for the READER/caller side.
    Returns True ONLY on a definitive ICMP port-unreachable (ConnectionRefusedError from a
    connected-UDP send/recv on Linux) -- i.e. we KNOW nothing is bound. Returns False on any
    other outcome (a reply, a timeout, or an unusable socket): SRT listeners do not answer our
    non-SRT probe bytes, so 'not refused' must NOT be read as 'a listener is up' -- only as 'not
    provably empty'. This asymmetry is deliberate: the reader fails fast only when it is CERTAIN
    the tap is dead, and otherwise lets ffmpeg try (a failed grab NO-SIGNALs, never crashes).
    Never raises."""
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    except OSError:
        return False
    try:
        s.settimeout(timeout)
        s.connect((host, port))
        s.send(b"\x00")
        try:
            s.recv(64)
        except ConnectionRefusedError:
            return True
        except (socket.timeout, OSError):
            return False
        return False
    except ConnectionRefusedError:
        return True
    except OSError:
        return False
    finally:
        s.close()


def assert_safe_to_start(url):
    """LAUNCH-PATH GUARD. Raises UnsafeTapError if `url` would start an SRT output in a
    crash-prone mode (caller / rendezvous -- both OPEN a connection at start and can fail it,
    which is exactly what crashed OBS on 2026-07-19). A LISTENER URL always passes: its bind
    never fails on a missing peer. Returns None on success.

    This is the enforced redesign: for the A/V-sync tap the OBS-side output MUST be a listener;
    the player connects as the caller. The message hands back the recommended listener URL."""
    parse_srt_target(url)  # validates it is a well-formed srt://host:port URL first
    mode = srt_mode(url)
    if mode == "listener":
        return None
    try:
        _, port = parse_srt_target(url)
        suggestion = recommend_tap_url(port=port)
    except ValueError:
        suggestion = recommend_tap_url()
    raise UnsafeTapError(
        f"SRT tap in {mode!r} mode is crash-prone: an unreachable peer fails the output start and "
        f"has crashed OBS (#802). Use LISTENER mode for the OBS-side tap instead: {suggestion} "
        f"(the player connects as the caller)."
    )


def reader_should_grab(url, timeout=1.0):
    """READER/caller side (e.g. av_sync_measure.py --grab, which pulls FROM the tap as a caller).
    Returns (ok, reason): (False, 'NO-SIGNAL: ...') ONLY when the tap is PROVABLY dead (a
    definitive UDP refusal), so the reader can short-circuit to a clean NO-SIGNAL (#814 family)
    instead of a doomed ffmpeg connect; (True, 'ok') otherwise -- including the indeterminate
    case, so a live-but-quiet SRT listener is never false-rejected. Never raises."""
    try:
        host, port = parse_srt_target(url)
    except ValueError:
        # Not an srt:// URL (rtmp://, a file, ...): this guard does not apply -> let it through.
        return True, "ok (non-srt url; reader preflight skipped)"
    if probe_udp_port_refused(host, port, timeout=timeout):
        return False, f"NO-SIGNAL: nothing listening at {host}:{port} (tap not up)"
    return True, "ok"


def _cli(argv=None):
    ap = argparse.ArgumentParser(description="SRT-tap safety guard + listener-mode recommender (#802)")
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--check", metavar="URL",
                   help="assert URL is SAFE to START as an OBS output (listener mode); "
                        "exit 2 + reason if it would start in a crash-prone caller/rendezvous mode")
    g.add_argument("--reader-probe", metavar="URL",
                   help="reader/caller side: exit 3 + NO-SIGNAL if the tap is provably not up")
    g.add_argument("--recommend", nargs="?", type=int, const=SRT_TAP_DEFAULT_PORT, metavar="PORT",
                   help="print the canonical crash-safe LISTENER tap URL for PORT "
                        f"(default {SRT_TAP_DEFAULT_PORT})")
    args = ap.parse_args(argv)

    if args.recommend is not None:
        print(recommend_tap_url(port=args.recommend))
        return 0
    if args.check is not None:
        try:
            assert_safe_to_start(args.check)
        except UnsafeTapError as exc:
            print(f"UNSAFE: {exc}")
            return 2
        except ValueError as exc:
            print(f"UNSAFE: {exc}")
            return 2
        print(f"SAFE: {args.check} is a listener tap -- start cannot fail on a missing peer")
        return 0
    if args.reader_probe is not None:
        ok, reason = reader_should_grab(args.reader_probe)
        print(reason)
        return 0 if ok else 3
    return 0


if __name__ == "__main__":
    sys.exit(_cli())
