---
paths:
  - "tests/*.rs"
  - "Cargo.toml"
---

<!-- Moved verbatim from CLAUDE.md (issue 1552). -->

## GOTCHA — one failing test binary makes `cargo test` SKIP the remaining binaries (a second RED hides)

`cargo test` stops scheduling not-yet-started test binaries after a binary fails ("waiting for
other jobs to finish") — so a run that shows ONE failure is NOT a complete accounting: another
already-RED test file later in the schedule silently never ran. Live incident (2026-07-16, #792
session): the full suite showed only `obs_self_heal_install` failing; after fixing it, a SECOND
pre-existing failure surfaced (`setup_imag_guards`, stale since the #783 same-source pivot the
day before). Both had sat unnoticed because the event-mode hotfix sessions never ran the full
suite. Rules: (1) after fixing a failure, ALWAYS re-run the FULL suite — never conclude "now
green" from the first fix; (2) a hotfix session that skips the full suite leaves landmines for
the next session — run it before ending the session even when CI is deliberately not triggered
(count `test result: ok` lines and expect the full binary count, currently ~156).
