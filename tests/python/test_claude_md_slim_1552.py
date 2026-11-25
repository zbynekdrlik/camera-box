"""issue 1552: CLAUDE.md loads on every turn, so it stays a lean router under 20 KB.

Area procedures and gotchas live in path-scoped `.claude/rules/<area>.md`; every rule file the
router or the every-ticket pointers name must exist and be path-scoped."""

import os
import re
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CLAUDE_MD = os.path.join(REPO, "CLAUDE.md")
RULES = os.path.join(REPO, ".claude", "rules")
SIZE_LIMIT = 20_000


def _read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


class ClaudeMdSlim(unittest.TestCase):
    def test_claude_md_stays_under_the_size_limit(self):
        size = len(_read(CLAUDE_MD).encode("utf-8"))
        self.assertLess(size, SIZE_LIMIT,
                        f"CLAUDE.md is {size} bytes; move area text verbatim into a path-scoped "
                        "`.claude/rules/<area>.md` and keep one router line (issue 1552)")

    def test_every_named_rule_exists_and_is_path_scoped(self):
        text = _read(CLAUDE_MD)
        named = set()
        for line in text.splitlines():
            if not line.startswith("- ") or "→" not in line or ".claude/skills/" in line:
                continue
            target = line.rsplit("→", 1)[1]
            named.update(re.findall(r"([a-z0-9][a-z0-9_.-]*\.md)", target))
        named = {n for n in named if not n.startswith("docs")}
        self.assertGreater(len(named), 150, "the router lists the rule files")
        for name in sorted(named):
            path = os.path.join(RULES, name)
            self.assertTrue(os.path.isfile(path), f"router names a missing rule: {name}")
            self.assertTrue(_read(path).startswith("---\npaths:\n"), f"{name} is not path-scoped")


if __name__ == "__main__":
    unittest.main()
