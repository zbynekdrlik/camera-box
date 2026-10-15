#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines two read-only readers of the strih OBS scene collection; no
# top-level statements) -- the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail`
# here: sourcing runs in the CALLER's shell, so strict mode would leak into it.
#
# scripts/lib/strih-obs-collection.sh -- the reads behind verify-strih item 28 (issue 1317 collection
# hygiene, REPORT-ONLY). Moved verbatim out of verify-strih.sh by issue 1399, so the gate stays under its
# ~1000-line budget when a new item is added. Read-only: provisioning never rewrites the owner's
# collection. The verdict itself stays strih_collection_hygiene_verdict (scripts/lib/strih-provision.sh).

# strih_active_collection_json OBS_CONFIG_DIR -> the ACTIVE scene collection JSON path: the
# `SceneCollectionFile=` of global.ini when that file exists, else the newest basic/scenes/*.json (the
# `*.json` glob already excludes the `*.json.bak*` backups). Prints nothing when there is none; rc 0.
strih_active_collection_json() {
  local base="${1:?obs config dir required}" name json="" cj
  name="$(sed -n 's/^SceneCollectionFile=//p' "${base}/global.ini" 2>/dev/null | head -1 || true)"
  if [ -n "$name" ] && [ -f "${base}/basic/scenes/${name}.json" ]; then
    json="${base}/basic/scenes/${name}.json"
  else
    for cj in "${base}/basic/scenes/"*.json; do
      [ -e "$cj" ] || continue
      if [ -z "$json" ] || [ "$cj" -nt "$json" ]; then json="$cj"; fi
    done
  fi
  printf '%s' "$json"
}

# strih_collection_hygiene_counts COLLECTION_JSON -> "SHADER LUA": the number of `shader_filter` objects
# anywhere in the collection and the number of `scripts-tool` module entries. Prints nothing when the
# file is not parseable JSON (the caller NOTEs "could not parse"); rc 0.
strih_collection_hygiene_counts() {
  python3 - "${1:?collection json required}" <<'PYHY' 2>/dev/null || true
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)   # print nothing -> caller NOTEs "could not parse"
def count_id(o, val):
    n = 0
    if isinstance(o, dict):
        if o.get("id") == val:
            n += 1
        for v in o.values():
            n += count_id(v, val)
    elif isinstance(o, list):
        for v in o:
            n += count_id(v, val)
    return n
shader = count_id(d, "shader_filter")
lua = 0
mods = d.get("modules", {}) if isinstance(d, dict) else {}
st = mods.get("scripts-tool") if isinstance(mods, dict) else None
if isinstance(st, list):
    lua = len(st)
elif isinstance(st, dict):
    inner = st.get("scripts")
    lua = len(inner) if isinstance(inner, list) else (1 if st else 0)
print("%d %d" % (shader, lua))
PYHY
}
