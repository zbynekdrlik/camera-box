#!/usr/bin/env bash
#
# install-git-hooks.sh — install the repo's local git hooks (#185).
#
# Installs a pre-push hook that runs scripts/purge-target.sh, so the shared dev1 Cargo target/ is
# bounded automatically before each push (it purges when target/ crosses the size budget, and is
# safe — it skips while a live E2E has probe binaries executing). This is the BACKSTOP to the root
# fix in CLAUDE.md (cheap checks run on DEFAULT features only, never --features probe). See #185.
#
# Idempotent: re-running overwrites the managed hook with the current version. Run once per checkout:
#   scripts/install-git-hooks.sh
#
# Git hooks are NOT versioned by git itself (.git/hooks is local) — that is why this installer is a
# tracked script: it materialises the hook into .git/hooks from the repo, so every checkout can opt in.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOKS_DIR="$REPO_ROOT/.git/hooks"
PRE_PUSH="$HOOKS_DIR/pre-push"

mkdir -p "$HOOKS_DIR"

cat > "$PRE_PUSH" <<'HOOK'
#!/usr/bin/env bash
# Managed by scripts/install-git-hooks.sh (#185) — bounds the shared dev1 Cargo target/.
# Purges target/ when it crosses the size budget (skips while a live E2E is running).
# Non-blocking: a purge failure must never stop a push.
set -uo pipefail
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
"$REPO_ROOT/scripts/purge-target.sh" || true
exit 0
HOOK

chmod +x "$PRE_PUSH"
echo "install-git-hooks.sh: installed pre-push hook -> $PRE_PUSH"
echo "install-git-hooks.sh: it runs scripts/purge-target.sh (bounds target/ to \${THRESHOLD_MB:-4096} MB)."
