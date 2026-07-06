#!/usr/bin/env bash
# scripts/lib/cli-log.sh — shared CLI color/log-line helpers (#559).
#
# scripts/deploy-fleet.sh, scripts/upgrade-fleet-ndi.sh, and scripts/verify-fleet.sh each carried
# their OWN byte-identical copy of this 5-line block (ANSI colors + log()/info()/warn()/err()).
# scripts/verify-fleet.sh (#552) was the third copy — extracting it here (same discipline as
# scripts/lib/ndi-alive.sh, #451) means the three scripts can never again drift out of sync on
# their log-line format the way #445 let ndi-alive's grep patterns drift before it was shared.
#
# Source-only: this file defines the color variables + four functions and performs no side
# effects on its own.
#
# log()  -- GREEN  "[+] msg"  (stdout) -- a completed step
# info() -- BLUE   "[*] msg"  (stdout) -- a status line
# warn() -- YELLOW "[!] msg"  (stdout) -- a recoverable problem
# err()  -- RED    "[ERROR] msg" (stderr) -- a failure

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
log()  { echo -e "${GREEN}[+]${NC} $*"; }
info() { echo -e "${BLUE}[*]${NC} $*"; }
warn() { echo -e "${YELLOW}[!]${NC} $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }
