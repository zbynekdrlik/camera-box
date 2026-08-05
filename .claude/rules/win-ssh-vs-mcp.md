---
paths:
  - "scripts/lib/win-ssh-exec.sh"
  - "scripts/lib/obs-session-visibility.sh"
  - "scripts/lib/audio-presence-preflight.sh"
  - "scripts/obs-session-watchdog.sh"
  - "scripts/recording-e2e.sh"
  - "scripts/recording-verdict-on-strih.sh"
  - "scripts/recording-verdict-on-stream.sh"
  - "scripts/launch-obs-genlock.sh"
  - "scripts/rig-mode.sh"
---

# ssh vs win-* MCP on the Windows boxes — the TWO-CONTEXT rule (hard, user-mandated)

The user has mandated this repeatedly for months; every violation is a live incident
(#859 ssh-launched OBS reaped at disconnect; issue 958 OBS invisible to the operator; PR #989's
gate false-failing a HEALTHY rig 3× because an ssh probe asserted `MainWindowTitle`). The
confusion always comes from mixing two contexts that have DIFFERENT capabilities:

## Context A — an agent session (Claude, interactive or autonomous)

**ALWAYS the win-* MCP (`mcp__win-strih__*` / `mcp__win-stream-snv__*`). NEVER ssh.**
The MCP server lives IN session 1 (the operator desktop): it sees windows, takes screenshots,
launches GUI apps that survive, reads true `MainWindowTitle`. If the MCP is unreachable — STOP
and alert; do NOT fall back to ssh (airuleset `mcp-error-handling`). The only sanctioned
agent-side ssh uses are FILE COPY (scp, #701) and headless CLI execution
(`recording-verdict.exe` decode, #703) — never anything GUI/desktop-session-dependent.

## Context B — headless automation (CI gate on dev1, systemd watchdog timers)

No MCP exists here — a bash job cannot call MCP tools, so ssh is the ONLY transport. That does
NOT lift the session physics:

- **An ssh shell on Windows runs in session 0.** It can NEVER see session-1 windows:
  `Process.MainWindowHandle`/`MainWindowTitle` go through `EnumWindows`, which only enumerates
  the calling process's own window station — cross-session the handle is ALWAYS `0` and the
  title ALWAYS empty **on a perfectly healthy box** (proven live on both strih + stream,
  2026-08-05, issue 958 comment 5191660073).
- **Headless probes may therefore FAIL only on session-agnostic signals:** process count,
  `SessionId` (compare against `explorer.exe`'s session, never a hardcoded 1), WorkingSet,
  listening ports, log mtime. A window-title/handle assertion from ssh is a false-negative
  by construction — context-gate it: require the title ONLY when the probe's own
  `(Get-Process -Id $PID).SessionId` equals the target's (i.e. when pasted into the MCP Shell).
- **NEVER launch or manipulate a GUI app over plain ssh** (it lands invisible in session 0
  and/or dies at disconnect, #859). Launch path is the MCP Shell; no-MCP fallback is the
  `Invoke-CimMethod Win32_Process Create` breakaway (`.claude/rules/rig-state-inspection.md`).
  `schtasks /it` is a documented DEAD END on these boxes (`Element not found` / result 267011).

## Litmus test before writing ANY Windows-touching line

"Does this operation depend on the DESKTOP (windows, screen, GUI launch, input)?"
- YES → Context A only (MCP; or CIM breakaway when no MCP). From headless code: don't do it —
  gate on session-agnostic signals instead.
- NO (file copy, CLI exe, service/process/port/registry query) → ssh via
  `scripts/lib/win-ssh-exec.sh` is fine in both contexts.

AHK note: `AutoHotkey64` has an EMPTY `MainWindowTitle` even IN session 1 (tray script, no main
window) — never assert a title on it in any context; presence + SessionId only.
