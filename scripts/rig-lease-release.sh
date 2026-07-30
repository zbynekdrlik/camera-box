#!/usr/bin/env bash
set -euo pipefail

# #830: release the shared cross-repo rig lease. Run as its OWN workflow step with `if: always()`
# so it fires on the success path, on a failure in ANY earlier step, AND on cancellation -- never
# only on scripts/rig-busy-gate.sh's own success path (that script releases the lease itself on
# its OWN failure paths via trap, but never on the success path -- the lease must stay held for
# the recording step that follows it in the same job). Safe no-op if we never held the lease (it
# was never acquired, e.g. a docs-only-skip run, or the gate failed fast before acquiring).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/rig-lease.sh
. "$HERE/lib/rig-lease.sh"

rig_lease_release "${RIG_LEASE_RUN_ID:-${GITHUB_RUN_ID:-local}}"
