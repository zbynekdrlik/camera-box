#!/usr/bin/env python3
"""issue 1404 -- connect each sampler NDI receiver to an address this box can actually reach.

WHY (read live on strih-snv 3.11.2026): avahi lists every RESOLUME-SNV source twice, on the rig NIC
(enp7s0, 10.77.9.201) and on the issue-1485 audio-VLAN leg (enp7s0.70, 10.77.7.107). The leg keeps
10.77.7.0/24 in routing table 70 only (`ip route get 10.77.7.107` = via 10.77.8.1 on enp7s0), so a TCP
connect to the VLAN address goes to the router and never completes. libndi (mDNS through avahi) takes
one of the two addresses when the receiver is created and never takes the other: the sampler's
`RESOLUME-SNV (SP-program)` receiver sat in SYN-SENT to 10.77.7.107:5961 for 3 h (connections 0, the
CG arm UNKNOWN), while fresh probes got 10.77.9.201 four times out of five.

So the sampler names the address itself: `pick_url` takes the source's avahi records
(`avahi-browse -tpr _ndi._tcp`, the box's own daemon, read-only) and keeps an address the MAIN
routing table reaches on-link (`ip -4 -o route get` without ` via `); with none it returns None and
the receiver is created by name as before. `PinnedReceiver` creates the receiver with that URL (the
name kept) and re-resolves while it has had no connection for REPIN_AFTER_S (a sender restarted on
another port, or no on-link record at the start), every REPIN_AFTER_S at most; still without a
connection after RECREATE_AFTER_S at the SAME url, it re-creates the receiver there anyway (live
3.11.2026 16:51: a cg-obs connection that dropped came back in SYN-SENT to the VLAN address and the
SDK never left it), at most every RECREATE_AFTER_S, each re-create logged; after RECREATE_FRUITLESS_MAX
re-creates without a connection (the CG box away, the sender off) every RECREATE_BACKOFF_S, until a
connection resets it. A failed create counts as an attempt. Only the capture thread
calls capture() (and so re-creates the receiver); connections() from the consumer thread takes the
same lock, so a receiver is never destroyed under either call.
"""
from __future__ import annotations

import os
import re
import subprocess
import threading
import time
from typing import Callable, NamedTuple

PIN_ENV = "PROGRAM_AUDIO_NDI_PIN"   # "0" = create every receiver by name only (tests, a rollback)
REPIN_AFTER_S = 15.0
RECREATE_AFTER_S = 60.0  # no connection this long at the SAME url: re-create the receiver anyway
RECREATE_FRUITLESS_MAX = 3   # after this many re-creates without a connection ...
RECREATE_BACKOFF_S = 300.0   # ... the next ones wait this long (a sender away / off)
AVAHI_TIMEOUT_S = 5.0
ROUTE_TIMEOUT_S = 2.0
NDI_SERVICE = "_ndi._tcp"


class AvahiRecord(NamedTuple):
    interface: str
    name: str
    address: str
    port: int


def unescape_avahi(text: str) -> str:
    """avahi-browse -p prints a service name with `\\DDD` DECIMAL byte escapes (`\\032` = space,
    `\\040` = '(') and `\\\\` / `\\.` for the escaped characters themselves."""
    out = bytearray()
    i = 0
    raw = text.encode("utf-8")
    while i < len(raw):
        if raw[i] == 0x5C and i + 4 <= len(raw) and raw[i + 1:i + 4].isdigit():
            out.append(int(raw[i + 1:i + 4]) & 0xFF)
            i += 4
        elif raw[i] == 0x5C and i + 1 < len(raw):
            out.append(raw[i + 1])
            i += 2
        else:
            out.append(raw[i])
            i += 1
    return out.decode("utf-8", errors="replace")


def parse_avahi(text: str) -> list[AvahiRecord]:
    """The resolved IPv4 records (`=;iface;IPv4;name;type;domain;host;address;port;txt`)."""
    recs = []
    for line in text.splitlines():
        f = line.split(";")
        if len(f) < 9 or f[0] != "=" or f[2] != "IPv4" or f[4] != NDI_SERVICE:
            continue
        try:
            port = int(f[8])
        except ValueError:
            continue
        if not re.fullmatch(r"[0-9]{1,3}(\.[0-9]{1,3}){3}", f[7]) or not 0 < port < 65536:
            continue
        recs.append(AvahiRecord(f[1], unescape_avahi(f[3]), f[7], port))
    return recs


def on_link_main(route_get_output: str) -> bool:
    """`ip -4 -o route get <addr>` printed a direct route of the main table (no gateway)."""
    line = route_get_output.strip()
    return bool(line) and " via " not in f" {line} " and not line.startswith(("unreachable", "prohibit",
                                                                             "blackhole", "throw"))


def pick_url(source: str, records, on_link: Callable[[str], bool]) -> str | None:
    """`<address>:<port>` of the first record (in address order) of `source` the box reaches on-link,
    or None (no record, or none on-link: let the SDK resolve by name)."""
    seen = sorted({(r.address, r.port) for r in records if r.name == source})
    for addr, port in seen:
        if on_link(addr):
            return f"{addr}:{port}"
    return None


def _run(cmd, timeout_s: float) -> str:
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout_s, check=False).stdout
    except (OSError, subprocess.SubprocessError):
        return ""


def resolve_url(source: str, *, run: Callable[..., str] = _run) -> str | None:
    """pick_url over the box's own avahi records and its main routing table."""
    recs = parse_avahi(run(["avahi-browse", "-tpr", NDI_SERVICE], AVAHI_TIMEOUT_S))
    return pick_url(source, recs, lambda a: on_link_main(run(["ip", "-4", "-o", "route", "get", a],
                                                             ROUTE_TIMEOUT_S)))


class PinnedReceiver:
    """A receiver created at a resolved URL (`make(name, url)`, url None = by name), re-created at a
    new URL after REPIN_AFTER_S without a connection. Same capture / connections / close interface as
    program_audio_ndi.NdiAudioReceiver."""

    def __init__(self, source: str, make: Callable[[str, str | None], object], *,
                 resolve: Callable[[str], str | None] | None = None,
                 mono: Callable[[], float] = time.monotonic, repin_after_s: float = REPIN_AFTER_S,
                 recreate_after_s: float = RECREATE_AFTER_S,
                 log: Callable[[str], None] = lambda _m: None):
        self.source_name = source
        self._make = make
        self._resolve = resolve if resolve is not None else resolve_url
        self._mono = mono
        self._repin_after_s = repin_after_s
        self._log = log
        self._recreate_after_s = max(recreate_after_s, repin_after_s)
        self._lock = threading.Lock()
        self.url = self._resolve_safely()
        self._recv = make(source, self.url)
        self._log_url("created")
        self._since_unconnected: float | None = None
        self._last_resolve = mono()
        self._last_create = self._last_resolve
        self._fruitless = 0          # re-creates since the last connection

    def _resolve_safely(self) -> str | None:
        try:
            return self._resolve(self.source_name)
        except Exception as exc:  # noqa: BLE001 -- a failed resolution falls back to the name
            self._log(f"program-audio sampler: ERROR resolving {self.source_name!r}: {exc!r} -- by name")
            return None

    def _log_url(self, what: str) -> None:
        where = self.url or "by name (no on-link avahi record)"
        self._log(f"program-audio sampler: NDI receiver {self.source_name!r} {what} at {where}")

    def capture(self, timeout_ms: int):
        block = self._recv.capture(timeout_ms)
        self._maybe_repin()
        return block

    def _maybe_repin(self) -> None:
        now = self._mono()
        if self.connections() > 0:
            self._since_unconnected = None
            self._fruitless = 0
            return
        if self._since_unconnected is None:
            self._since_unconnected = now
            return
        unconnected_s = now - self._since_unconnected
        if unconnected_s < self._repin_after_s:
            return
        backed_off = self._fruitless >= RECREATE_FRUITLESS_MAX
        url = self.url
        # avahi + ip at most every REPIN_AFTER_S (every RECREATE_BACKOFF_S once backed off: the browse
        # blocks this capture thread up to AVAHI_TIMEOUT_S)
        if now - self._last_resolve >= (max(RECREATE_BACKOFF_S, self._repin_after_s) if backed_off
                                        else self._repin_after_s):
            self._last_resolve = now
            url = self._resolve_safely()
            if url is None and self.url is not None:
                # an avahi timeout / an empty browse is no new information: never drop a pinned url
                # for a by-name receiver libndi may put on the VLAN address again (review round 2)
                url = self.url
        wait = max(RECREATE_BACKOFF_S, self._recreate_after_s) if backed_off else self._recreate_after_s
        first_try_at_new_url = url != self.url and not self._fruitless
        if now - self._last_create < (self._repin_after_s if first_try_at_new_url else wait):
            return
        if url == self.url and unconnected_s < self._recreate_after_s:
            return
        # One attempt per interval, also when it fails (review round 1: a failed create was retried
        # on every capture call).
        self._last_create = now
        self._fruitless += 1
        try:
            new = self._make(self.source_name, url)
        except (OSError, RuntimeError) as exc:  # keep the old receiver; the next try after the interval
            self._log(f"program-audio sampler: ERROR re-creating {self.source_name!r} at {url}: {exc}")
            return
        with self._lock:
            old, self._recv, self.url = self._recv, new, url
        old.close()
        self._since_unconnected = None
        self._log_url(f"re-created after {unconnected_s:.0f} s without a connection")

    def connections(self) -> int:
        with self._lock:
            return int(self._recv.connections())

    def close(self) -> None:
        with self._lock:
            self._recv.close()


def pinning_enabled() -> bool:
    return os.environ.get(PIN_ENV, "1") != "0"


def open_receiver(source: str, make: Callable[[str, str | None], object], *,
                  log: Callable[[str], None] = lambda _m: None):
    """The sampler's one way to create a receiver: a PinnedReceiver, or with $PROGRAM_AUDIO_NDI_PIN=0
    `make(source, None)` itself (by name, exactly as before the pin)."""
    if not pinning_enabled():
        return make(source, None)
    return PinnedReceiver(source, make, log=log)
