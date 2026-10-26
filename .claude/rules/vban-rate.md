---
paths:
  - "scripts/vban_rate.py"
  - "tests/python/test_vban_rate_1372.py"
---

# Inter-PC VBAN rate + loss — by FRAME COUNTER, never by packet count (issue 1372 part C)

## The measurement (`scripts/vban_rate.py`, pure, pytest)

- **Rate = the VBAN frame counter**, never the packet count. Each header carries `nuFrame` (u32 LE at
  byte 24); a packet's sample index is `nuFrame x (nbs+1)`; the stream clock is the least-squares
  slope of that index against capture time, pooled over counter segments (common slope, one
  intercept per segment). A lost packet leaves a hole but moves no point, so loss never biases rate.
  The count method read −132.9 ppm for a +18.1 ppm stream with 5 lost packets (#1367 comment
  5832526338) — a count mixes two faults into one number.
- **The fit is ONE-SIDED TRIMMED (review round 1).** Delay only makes an arrival LATER, so a plain
  fit over every arrival was biased by one stall-then-burst (+65.7 ppm for a single 500 ms stall in
  60 s). Points later than the line by more than `max(4 × 1.4826 × MAD, 2 ms)` above the median
  lateness are dropped and the line refitted (≤ 4 rounds). Symmetric jitter is bounded and keeps
  every point. A per-window lower-envelope fit was tried first and REJECTED: with ~190 packets per
  1 s window the minimum is itself noisy at ~0.2 ms, which is ±10 ppm over 60 s.
- **The stderr is cluster-robust (review round 4):** packets arriving < 1 ms apart were released as
  one burst and share one timing error, so each arrival cluster is ONE observation (a sandwich
  estimator). A per-packet stderr was ~2x too small on obs-vban bursts, and a true 0 ppm clock graded
  FAULT on 2/40 seeds. With ±20 ms burst jitter a 60 s capture resolves only ~14 ppm (1 σ), so such a
  stream honestly grades UNCERTAIN; the live strih-snv streams jitter ~0.4-2.7 ms rms.
- **Known limit:** a sender that restarts less than a second after the capture began, onto counters
  within ~50 ms worth of its head, can be merged under heavy jitter (its continuation is only
  ~50 ms late). A real restart from a running sender (a large counter) is always a jump. A sender
  that resets its counter with ZERO send pause while its old packets are still reordered in flight
  can book phantom loss in the old segment (review round 5: 11/20 fohabl-model seeds at 0 ms pause,
  0/20 at >= 5 ms). Not handled: a VBAN sender reset is a process restart, never under 5 ms.
- **Grading by the 2-stderr interval (review round 3):** FAULT when `|rate| − 2σ` is outside the
  bound (a gross fault is a FAULT however noisy the fit), OK when `|rate| + 2σ` stays inside it,
  UNCERTAIN when the interval straddles it (no page, no recovery; loss is still graded).
- **Loss** = per segment `(max − min + 1) − unique`: a pktmon duplicate (the same packet at several
  components) or a reorder is never loss. **Jump** = a step back > 64 frames, a step back to a
  counter already seen more than 50 ms earlier or below everything the segment has seen (a restart
  right after the capture began), or a step forward beyond 2 × the frames the elapsed time explains
  + 64 (a sender restart). A real outage advances the counter by ~the elapsed frames and therefore
  counts as LOSS, not a jump. **Every jump candidate is confirmed by lookahead (review rounds 2-3):**
  counters the segment already holds, counters still in flight around the head (an ordinary
  reorder, review round 4) and further stragglers far below the head (a second late original,
  round 5) are skipped, and the first counter ABOVE the head decides. If it continues the OLD head AND
  (when the skipped counters cover the whole gap back to the candidate, i.e. a possible restart
  re-send) arrives on time for that step (within 50 ms of step / rate after the last accepted packet),
  the candidate is a straggler or a late duplicate: a duplicate stays a duplicate, a straggler fills its
  own hole, and a lone far-ahead counter or a pre-segment packet is dropped, so there is never phantom
  loss. If it continues from the candidate, it is a restart. A restart whose re-sent counters collide
  with the old ones is still a restart, because its continuation arrives LATE for the old head. A
  reorder in a segment's first 50 ms (2,0,1 at capture start) is never a jump.
- **Step limits use the last ACCEPTED packet (review round 1).** The step limits use the arrival
  of the last ACCEPTED packet, so a late duplicate never shrinks them (a duplicate near the end of
  an outage cannot hide the outage's loss), and the lookahead treats the rest of a late-duplicate
  burst as the old stream.
- Reported alongside: `max_gap_ms`, the fit residual (arrival jitter — obs-vban bursts), the slope
  stderr. A stream shorter than `--min-span-s` (default 20 s) reads SHORT, never a FAULT.
- **Formats:** classic pcap (either byte order, µs/ns) LINUX_SLL2 276 (`tcpdump -i any`), SLL 113,
  EN10MB 1 (+802.1Q), raw IPv4; pcapng (pktmon `etl2pcap`, any `if_tsresol`). IPv4/UDP only;
  fragments skipped. **snaplen ≥ 96** or the 28-byte header is cut: every-header-cut =
  `CAPTURE_TRUNCATED`, a loud config error, never an empty OK.
- `--dst IP` keeps only streams ARRIVING at the receiver (a strih-snv capture also sees the hub's own
  strih-snv → camN streams, which say nothing about a peer's clock). `--json` / `--tsv` for tools.

## The dev1 watchdog was RETIRED (26.10.2026, ROZHODNUTÉ on issue 1372)

`scripts/vban-rate-alert-watchdog.sh` + its unit (a 60 s tcpdump on strih-snv pulled to dev1 every
pass) shipped DISABLED and never ran. It was removed because:

- **Each receiver measures its own streams now.** The obs-vban RECEIVE source on the stream box
  (issue 1439, `obs-vban-receive.md`) prints `obs-vban receive:` every 10 s with `received` / `lost` /
  `late` / `re_anchors` / `concealed` and `sender_ppm` (a 64 s least-squares fit of the sender
  clock) -- the same rate + loss, measured where the program audio is played out, not from a capture.
  The strih-snv hub reports its VBAN legs (stalls, lost ticks, the worst leg) on `/api/state`.
- **The release gate is BLOCKING on it.** The full-path E2E's program-audio continuity gate
  (`scripts/lib/program-audio-continuity.sh`) fails a run on those receive deltas.
- **No rig services or rig data on dev1** (owner rule), and the capture pulled ~10 MB/min over ssh.

What stays: `scripts/vban_rate.py` (the offline pcap / pktmon diagnosis CLI, and the ONE VBAN header
layout the issue-1460 bench planner `scripts/vban_emulator_plan.py` imports). A between-run page on
VBAN rate/loss, if ever wanted, reads the receiver's own status line on its box; do not bring back a
dev1 capture.

## Live baseline (25.9.2026, before part A)

`fohabl-strih` (10.77.7.30, 96 kHz, 103 samples/frame) at strih-snv: +10.3 ppm by the plain fit,
+7.7 ppm by the trimmed fit (its late tail was the bias), 39 lost / 40 s (1.05e-3) and 49 / 60 s
(8.8e-4) — the FOH VBAN OUT loss. The hub's own outputs: −0.55 ppm, 0 loss.

## Manual capture on a Linux receiver

`tcpdump -i any -s 96 -U -w <file> 'udp and udp[8:4] = 0x5642414e'` (the VBAN-magic BPF; without
it a 15 s capture was 192 MB of NDI UDP), then `python3 scripts/vban_rate.py analyze --dst <receiver IP>
<file>`. Linux adjtimex slews CLOCK_REALTIME, so on a dantesync-disciplined box the capture clock is
the disciplined clock. A Dante-clocked sender still differs from system time by dantesync's
`f_phase`.

## Windows side (pktmon)

The same parser reads a pktmon capture converted with `pktmon etl2pcap` (pcapng, EN10MB). Filter to
the NIC component or pktmon's per-component copies show up as `dup=` (harmless — dedup is by counter).
