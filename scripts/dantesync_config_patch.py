#!/usr/bin/env python3
"""dantesync_config_patch.py -- pure dantesync config.json patcher for issue 1297.

RESOLUME-SNV's dantesync carried NO ``system.phase_slew`` key, so it STEPS its UTC phase in
discrete jumps (the issue-1130 storm the rig boxes + mbc already cured by enabling phase_slew).
This helper turns the on-box flip into a reviewed, repeatable, text-only transform:

  * set the clock policy (issue 1372): for a dantesync 1.9.0+ node (the default) write
    ``system.clock_discipline = "ptp_phase_lock"`` -- the rate and phase from the PTP tick only,
    phase slew is not used -- and leave ``system.phase_slew`` exactly as it is (1.9.0 ignores it).
    For a pre-1.9.0 node (``legacy_phase_slew=True`` / ``--legacy-phase-slew``) keep the old flip,
    ``system.phase_slew.enabled = true``. Either way the nesting is created and every sibling key
    and the document's key order are preserved,
  * optionally repoint ``ntp_server`` -- PARAMETRISED, default UNCHANGED until the dantesync
    failover-list feature (zbynekdrlik/dantesync#111) lands and the master-chain target is decided
    on the ticket,
  * and REFUSE to touch ``ntp_server_mode`` -- a hard round-trip invariant (the "never two
    masters" rule, .claude/memory rig-timesync-single-authority): exactly one phase master per box.

It is I/O-free and pure so it is unit-tested directly (pytest, Tier-0 -- cargo is banned in this
repo, pytest runs freely). The on-box APPLY (backup -> write -> restart -> read-back) is emitted as
a text PowerShell program the supervisor pastes into the win-resolume MCP Shell (``emit_apply_program``
/ ``--emit-apply``); this module NEVER runs it and NEVER touches a box.
"""
from __future__ import annotations

import argparse
import json
import sys


class ConfigPatchError(ValueError):
    """Raised when the input is not a patchable dantesync config.json."""


def _nsm_fingerprint(cfg):
    """A canonical, order-independent fingerprint of ntp_server_mode (or None when absent)."""
    if "ntp_server_mode" not in cfg:
        return None
    return json.dumps(cfg["ntp_server_mode"], sort_keys=True)


def patch_config(text, ntp_server=None, legacy_phase_slew=False):
    """Return TEXT (a dantesync config.json) with the clock policy set, preserving every other key +
    order, optionally repointing ntp_server, and NEVER touching ntp_server_mode.

    The clock policy (issue 1372): by default ``system.clock_discipline = "ptp_phase_lock"`` (the
    dantesync 1.9.0 discipline; a ``legacy`` value is replaced) with ``system.phase_slew`` left
    untouched. ``legacy_phase_slew=True`` instead sets ``system.phase_slew.enabled = true`` for a
    pre-1.9.0 node, which has no clock_discipline and cures its step storm by slewing.

    Raises ConfigPatchError on invalid JSON, a non-object top level, a non-object ``system`` /
    ``system.phase_slew`` in legacy mode (we refuse to clobber an unexpected shape), an empty
    ntp_server, or if ntp_server_mode would change (defensive -- it never should, so a change
    means a bug)."""
    try:
        cfg = json.loads(text)
    except json.JSONDecodeError as exc:
        raise ConfigPatchError(f"not valid JSON: {exc}") from exc
    if not isinstance(cfg, dict):
        raise ConfigPatchError("top-level config is not a JSON object")

    nsm_before = _nsm_fingerprint(cfg)

    # The clock policy, creating the system nesting but preserving any sibling keys.
    system = cfg.get("system")
    if system is None:
        system = {}
        cfg["system"] = system
    elif not isinstance(system, dict):
        raise ConfigPatchError('"system" is present but is not a JSON object')
    if legacy_phase_slew:
        # A pre-1.9.0 node: system.phase_slew.enabled = true (the issue-1130 step-storm cure).
        phase_slew = system.get("phase_slew")
        if phase_slew is None:
            phase_slew = {}
            system["phase_slew"] = phase_slew
        elif not isinstance(phase_slew, dict):
            raise ConfigPatchError('"system.phase_slew" is present but is not a JSON object')
        phase_slew["enabled"] = True
    else:
        # dantesync 1.9.0+: the PTP phase lock; phase_slew is ignored by it and left as it is.
        system["clock_discipline"] = "ptp_phase_lock"

    # Optional ntp_server repoint. Default None => leave the existing value exactly as-is (the
    # repoint target is a deferred design decision, see the module docstring).
    if ntp_server is not None:
        if not isinstance(ntp_server, str) or not ntp_server.strip():
            raise ConfigPatchError("ntp_server must be a non-empty string")
        cfg["ntp_server"] = ntp_server

    # Hard invariant: ntp_server_mode is byte-identical before and after (never two masters).
    if _nsm_fingerprint(cfg) != nsm_before:
        raise ConfigPatchError("refusing to patch: ntp_server_mode changed (never two masters)")

    return json.dumps(cfg, indent=2) + "\n"


# The PowerShell apply program template. %-style fields are filled by emit_apply_program; the
# patched JSON is embedded in a SINGLE-quoted here-string (@'...'@) so no $ / backtick inside the
# JSON is ever interpolated, and written with an explicit no-BOM UTF-8 encoder (serde_json does
# not skip a BOM). Mirrors the mbc phase_slew flip recorded on issue 1265: backup -> write ->
# Stop-Service (wait for Stopped) + Start-Service -> read back :PORT/status.
_APPLY_PROGRAM_TEMPLATE = r"""$ErrorActionPreference = 'Stop'
$cfg = '{config_path}'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$bak = "$cfg.bak-$stamp"
Copy-Item -LiteralPath $cfg -Destination $bak -Force
Write-Host "backed up -> $bak"
$json = @'
{patched_json}
'@
[System.IO.File]::WriteAllText($cfg, $json, (New-Object System.Text.UTF8Encoding $false))
Write-Host "wrote patched config -> $cfg"
# Restart-Service reports 'stop failed' on this service (the stop takes ~10 s) and then never
# starts it again -- observed live on RESOLUME-SNV 2026-09-12, service left STOPPED. Stop, wait, start.
Stop-Service -Name '{service}' -Force -ErrorAction SilentlyContinue
$i = 0; while ((Get-Service -Name '{service}').Status -ne 'Stopped' -and $i -lt 30) {{ Start-Sleep -Seconds 1; $i++ }}
Start-Service -Name '{service}'
Write-Host "service {service}: $((Get-Service -Name '{service}').Status) after ${{i}}s stop-wait"
Write-Host "restarted service {service}; settling {settle}s before read-back"
Start-Sleep -Seconds {settle}
$status = Invoke-RestMethod -Uri 'http://localhost:{status_port}/status' -TimeoutSec 10
$status | ConvertTo-Json -Depth 8
"""


def emit_apply_program(
    config_path,
    patched_json,
    service="dantesync",
    status_port=8898,
    settle_s=8,
):
    """Return the EMIT-ONLY PowerShell program the supervisor pastes into win-resolume to APPLY
    PATCHED_JSON on-box: backup config.json.bak-<date> -> write (no-BOM UTF-8) -> Restart-Service
    -> read back :PORT/status. This module never executes it -- it returns text only."""
    if not isinstance(status_port, int) or status_port <= 0:
        raise ConfigPatchError("status_port must be a positive integer")
    if not isinstance(settle_s, int) or settle_s < 0:
        raise ConfigPatchError("settle_s must be a non-negative integer")
    return _APPLY_PROGRAM_TEMPLATE.format(
        config_path=config_path,
        patched_json=patched_json.rstrip("\n"),
        service=service,
        status_port=status_port,
        settle=settle_s,
    )


def _main(argv=None):
    ap = argparse.ArgumentParser(description="Patch a dantesync config.json (issue 1297).")
    ap.add_argument(
        "--emit-apply",
        action="store_true",
        help="emit the PowerShell APPLY program (supervisor pastes it into win-resolume) instead "
        "of the patched JSON",
    )
    ap.add_argument(
        "--legacy-phase-slew",
        action="store_true",
        help="a pre-1.9.0 dantesync node: set system.phase_slew.enabled=true instead of the 1.9.0 "
        "system.clock_discipline=ptp_phase_lock (issue 1372)",
    )
    ap.add_argument(
        "--ntp-server",
        default=None,
        help="repoint ntp_server to this value (default: leave the existing value unchanged)",
    )
    ap.add_argument(
        "--config-path",
        default=r"C:\ProgramData\dantesync\config.json",
        help="the on-box config.json path baked into the --emit-apply program",
    )
    ap.add_argument("--service", default="dantesync", help="the on-box service name to restart")
    ap.add_argument("--status-port", type=int, default=8898, help="the :PORT/status read-back port")
    args = ap.parse_args(argv)

    text = sys.stdin.read()
    try:
        patched = patch_config(text, ntp_server=args.ntp_server,
                               legacy_phase_slew=args.legacy_phase_slew)
    except ConfigPatchError as exc:
        print(f"dantesync_config_patch: {exc}", file=sys.stderr)
        return 1

    if args.emit_apply:
        sys.stdout.write(
            emit_apply_program(
                args.config_path,
                patched,
                service=args.service,
                status_port=args.status_port,
            )
        )
    else:
        sys.stdout.write(patched)
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
