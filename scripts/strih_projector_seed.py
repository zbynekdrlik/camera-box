#!/usr/bin/env python3
"""issue 1419 -- seed the operator's fullscreen Multiview projector into a strih box's scene collection.

strih-snv's collection saves one projector, `{"monitor": 0, "type": 4}` (type 4 = the Multiview,
fullscreen on monitor 0; OBS reopens it at start because user.ini [BasicWindow] SaveProjectors=true,
seeded by scripts/lib/strih-operator.sh). strih-pp's ported collection saved none, so its operator had
no Multiview at all (owner 13.10.2026: "cely obs setup ... aky mame v strih snv").

Operator-wins: an EMPTY `saved_projectors` list is seeded; any existing list (the operator's own
projectors, their geometry) is never touched. A missing or unreadable collection is refused. The write
is a temp file + rename in the same directory with the file's mode kept. Run it only while OBS is
stopped: OBS rewrites the collection from memory on exit.

  python3 scripts/strih_projector_seed.py --collection ~/.config/obs-studio/basic/scenes/strih-pp.json
"""
import argparse
import json
import os
import sys
import tempfile

MULTIVIEW_PROJECTOR = {"monitor": 0, "type": 4, "geometry": "", "alwaysOnTopOverridden": False}


def seeded(collection):
    """The collection dict with the Multiview projector seeded, or None when nothing changes."""
    if collection.get("saved_projectors"):
        return None
    out = dict(collection)
    out["saved_projectors"] = [dict(MULTIVIEW_PROJECTOR)]
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--collection", required=True, help="the scene collection json")
    a = ap.parse_args(argv)
    path = a.collection
    if os.path.islink(path):
        print("FAIL issue 1419: %s is a symlink -- refusing to write it" % path, file=sys.stderr)
        return 2
    try:
        with open(path, encoding="utf-8") as fh:
            collection = json.load(fh)
    except (OSError, ValueError) as e:
        print("FAIL issue 1419: the collection %s cannot be read (%s)" % (path, e), file=sys.stderr)
        return 2
    new = seeded(collection)
    if new is None:
        print("  saved projectors in %s: kept (%d present)" % (path, len(collection["saved_projectors"])))
        return 0
    mode = os.stat(path).st_mode & 0o777
    fd, tmp = tempfile.mkstemp(prefix=".collection.", dir=os.path.dirname(os.path.abspath(path)))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(new, fh, indent=4, ensure_ascii=False)
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        os.unlink(tmp)
        raise
    print("  saved projectors in %s: seeded the fullscreen Multiview on monitor 0" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
