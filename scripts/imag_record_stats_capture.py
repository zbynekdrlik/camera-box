#!/usr/bin/env python3
"""#1143 -- capture OBS's own record-session render stats for ONE imag recording from the imag OBS
log, and print them as the compact JSON `imag_record_encoder.parse_obs_record_stats` emits
(drawn/attempted/lagged frames + lagged_pct + max in-record render ms). The dev1 E2E harness runs
this AFTER StopRecord and passes the result to `recording-verdict --extract-partial imag
--record-render-stats`, so the imag verdict block carries the observer-effect proof (report-only).

Best-effort by design: ANY failure (imag unreachable, log missing, record window not found) prints
NOTHING and exits 0 -- the caller treats empty as "no record_render carried" and the observability
term is simply absent that run. It never aborts the extract (report-only).
"""
import argparse
import os
import pathlib
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import imag_record_encoder  # noqa: E402  (sibling pure module: parse_obs_record_stats)


def _ssh_log_window(host, basename):
    """SSH to HOST and print the newest OBS log's lines from the recording's 'Writing file' line
    through a few lines past its 'stopped' line (captures the in-record program-render-audit lines +
    the stop-stats block). basename is an OBS timestamp filename ('2026-08-19 14-31-54.mkv') -- no
    shell metacharacters but a space; single quotes are stripped defensively before embedding."""
    user = os.environ.get("IMAG_USER", "newlevel")
    pw = os.environ.get("IMAG_PW", "newlevel")
    b = basename.replace("'", "")
    remote = (
        "LOG=$(ls -t ~/.config/obs-studio/logs/*.txt 2>/dev/null | head -1); "
        "[ -n \"$LOG\" ] || exit 0; "
        "s=$(grep -n 'Writing file' \"$LOG\" | grep -F '" + b + "' | head -1 | cut -d: -f1); "
        "e=$(grep -n 'stopped' \"$LOG\" | grep -F '" + b + "' | head -1 | cut -d: -f1); "
        "[ -n \"$s\" ] && [ -n \"$e\" ] && sed -n \"${s},$((e+8))p\" \"$LOG\""
    )
    r = subprocess.run(
        ["sshpass", "-p", pw, "ssh", "-o", "StrictHostKeyChecking=no",
         "-o", "ConnectTimeout=10", f"{user}@{host}", remote],
        capture_output=True, text=True, timeout=30, check=False,
    )
    return r.stdout


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", required=True, help="imag-nb IP/host")
    ap.add_argument("--recording", required=True,
                    help="the imag recording path as it lives on imag (StopRecord's outputPath)")
    a = ap.parse_args()
    basename = pathlib.PurePosixPath(a.recording).name
    try:
        text = _ssh_log_window(a.host, basename)
    except Exception as e:  # noqa: BLE001 -- best-effort; a hiccup must never abort the extract
        sys.stderr.write(f"[imag-record-stats] WARN: could not read imag OBS log ({e})\n")
        return
    stats = imag_record_encoder.parse_obs_record_stats(text, basename)
    if stats:
        import json
        print(json.dumps(stats, separators=(",", ":")))
    else:
        sys.stderr.write(
            f"[imag-record-stats] no OBS stop-stats found for {basename} "
            "(record window not in the log?) — carrying no record_render this run\n")


if __name__ == "__main__":
    main()
