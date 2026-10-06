/* Issue 1367 -- the sequence-replay model of the DistroAV receiver's reset -> bind -> verify loop.
 *
 * NOT compiled on its own: tests/distroav_stale_duplicate_retarget_1367.rs prepends a stub
 * NDIlib_source_t plus the SHIPPED helpers and constants it lifts VERBATIM from
 * vendor/distroav/src/ndi-source.cpp (ndi_find_url_for_source_name, ndi_by_url_identity_mismatch,
 * ndi_force_by_name_after_frameless, ndi_identity_mismatch_action_1367,
 * ndi_url_exclusion_active_1367, GENLOCK_RECONNECT_STALE_NS, NDI_URL_EXCLUDE_TTL_NS), then compiles
 * the whole under -std=gnu99 -Wall -Wextra -Wformat=2 -Wconversion -Werror and parses the RESULT
 * lines. `legacy` runs the SAME loop with the pre-fix wiring (no retarget, no exclusion, every
 * mismatch forced BY-NAME); it must reproduce the live ~55 s / 6-mismatch loop, which is what keeps
 * the model honest. */
#define MS_NS 1000000ULL
#define S_NS 1000000000ULL
#define NAME "RESOLUME-SNV (SP-program)"
#define STALE "10.77.9.201:5961" /* the stale record; another LIVE sender owns this port now */
#define LIVE "10.77.9.201:5971"  /* our sender's new port */
#define OTHER "10.77.9.201:5981" /* a third live sender */

typedef struct {
	const char *tag;
	NDIlib_source_t reset_before[4]; uint32_t n_reset_before; /* the reset's fresh finder while the stale record lives */
	NDIlib_source_t reset_after[4]; uint32_t n_reset_after;   /* every finder after it aged out */
	NDIlib_source_t verify_first[4]; uint32_t n_verify_first; /* the first #1180 verify finder */
	NDIlib_source_t verify_later[4]; uint32_t n_verify_later; /* every later verify finder */
	uint64_t stale_ages_out_ns;   /* episode-relative */
	uint64_t live_delivers_from_ns; /* episode-relative: our sender delivers frames from here */
} scenario_t;

static bool same(const char *a, const char *b) { return a && b && strcmp(a, b) == 0; }

static unsigned long long ms(uint64_t ns) { return (unsigned long long)(ns / MS_NS); }

static void result(const char *tag, bool legacy, int attached, unsigned reset, uint64_t rel,
		   unsigned mismatches, unsigned retargets, unsigned by_name, unsigned stale_after,
		   unsigned chained, int wrong_sender, const char *excluded)
{
	printf("RESULT %s legacy=%d attached=%d reset=%u t_ms=%llu mismatches=%u retargets=%u by_name_resets=%u stale_binds_after_mismatch=%u chained_retargets=%u wrong_sender=%d excluded_after=%s\n",
	       tag, legacy ? 1 : 0, attached, reset, ms(rel), mismatches, retargets, by_name, stale_after,
	       chained, wrong_sender, excluded[0] ? excluded : "-");
}

static void run(const scenario_t *sc, bool legacy)
{
	const uint64_t t0 = 5ULL * S_NS; /* the monotonic clock is never 0 (0 = "nothing excluded") */
	uint64_t t = t0;
	char retarget[64] = "";
	char excluded[64] = "";
	uint64_t excluded_since = 0;
	bool force_by_name = false, prev_retarget = false, seen_mismatch = false;
	unsigned mismatches = 0, retargets = 0, by_name = 0, stale_after = 0, chained = 0, verifies = 0;
	for (unsigned reset = 1; reset <= 24; ++reset) {
		t += 100ULL * MS_NS; /* the reset block itself */
		uint64_t rel = t - t0;
		bool stale_listed = rel < sc->stale_ages_out_ns;
		const NDIlib_source_t *rl = stale_listed ? sc->reset_before : sc->reset_after;
		uint32_t nrl = stale_listed ? sc->n_reset_before : sc->n_reset_after;
		/* reset block: consume the force flag + the retarget, expire the exclusion */
		bool forced = force_by_name;
		force_by_name = false;
		char take[64];
		snprintf(take, sizeof take, "%s", retarget);
		retarget[0] = '\0';
		if (excluded[0] && !ndi_url_exclusion_active_1367(excluded_since, t, NDI_URL_EXCLUDE_TTL_NS)) {
			excluded[0] = '\0';
			excluded_since = 0;
		}
		const char *excl = (!legacy && excluded[0]) ? excluded : NULL;
		bool via_retarget = false;
		const char *bound = NULL;
		const char *mode = "BYNAME";
		if (!forced && !legacy && take[0]) {
			bound = take;
			mode = "RETARGET";
			via_retarget = true;
			retargets++;
		} else if (!forced) {
			bound = ndi_find_url_for_source_name(NAME, rl, nrl, excl);
			mode = bound ? "BYURL" : "BYNAME";
		}
		if (via_retarget && prev_retarget)
			chained++;
		prev_retarget = via_retarget;
		if (!bound) {
			/* BY-NAME: the SDK resolver follows the stale record while it is listed (live: no
			 * connection inside the stale window); once it aged out it reaches our sender. */
			by_name++;
			if (!stale_listed && rel >= sc->live_delivers_from_ns) {
				printf("%s reset %u t_ms=%llu BYNAME -> frames from " LIVE "\n", sc->tag, reset, ms(rel));
				result(sc->tag, legacy, 1, reset, rel, mismatches, retargets, by_name, stale_after, chained, 0, excluded);
				return;
			}
			printf("%s reset %u t_ms=%llu BYNAME -> no connection inside the stale window\n", sc->tag, reset, ms(rel));
			t += GENLOCK_RECONNECT_STALE_NS;
			force_by_name = ndi_force_by_name_after_frameless(false, false);
			continue;
		}
		if (seen_mismatch && same(bound, STALE))
			stale_after++;
		bool delivers = !same(bound, LIVE) || rel >= sc->live_delivers_from_ns;
		if (!delivers) {
			printf("%s reset %u t_ms=%llu %s %s -> frame-less (sender not delivering yet)\n", sc->tag, reset, ms(rel), mode, bound);
			t += GENLOCK_RECONNECT_STALE_NS;
			force_by_name = ndi_force_by_name_after_frameless(true, false);
			continue;
		}
		/* frames flow -> the one-shot #1180 identity verify (its own fresh finder) */
		t += 30ULL * MS_NS;
		const NDIlib_source_t *vl;
		uint32_t nvl;
		if (!stale_listed) {
			vl = sc->reset_after;
			nvl = sc->n_reset_after;
		} else if (verifies == 0) {
			vl = sc->verify_first;
			nvl = sc->n_verify_first;
		} else {
			vl = sc->verify_later;
			nvl = sc->n_verify_later;
		}
		verifies++;
		const char *vexcl = (!legacy && excluded[0] &&
				     ndi_url_exclusion_active_1367(excluded_since, t, NDI_URL_EXCLUDE_TTL_NS))
					    ? excluded
					    : NULL;
		const char *v = ndi_find_url_for_source_name(NAME, vl, nvl, vexcl);
		bool mm = ndi_by_url_identity_mismatch(bound, v);
		int action = legacy ? (mm ? 2 : 0) : ndi_identity_mismatch_action_1367(mm, v, via_retarget);
		if (mm) {
			mismatches++;
			seen_mismatch = true;
			if (!legacy) {
				snprintf(excluded, sizeof excluded, "%s", bound);
				excluded_since = t;
			}
		}
		if (action == 1) {
			snprintf(retarget, sizeof retarget, "%s", v);
			printf("%s reset %u t_ms=%llu %s %s -> MISMATCH (name maps to %s) -> RETARGET\n", sc->tag, reset, ms(rel), mode, bound, v);
			continue;
		}
		if (mm) {
			force_by_name = true;
			printf("%s reset %u t_ms=%llu %s %s -> MISMATCH (name maps to %s) -> BYNAME\n", sc->tag, reset, ms(rel), mode, bound, v);
			continue;
		}
		if (v && v[0] && !legacy) {
			excluded[0] = '\0';
			excluded_since = 0;
		}
		if (same(bound, LIVE)) {
			printf("%s reset %u t_ms=%llu %s %s -> frames, identity verified\n", sc->tag, reset, ms(t - t0), mode, bound);
			result(sc->tag, legacy, 1, reset, t - t0, mismatches, retargets, by_name, stale_after, chained, 0, excluded);
			return;
		}
		printf("%s reset %u t_ms=%llu %s %s -> WRONG SENDER ACCEPTED\n", sc->tag, reset, ms(rel), mode, bound);
		result(sc->tag, legacy, 0, reset, t - t0, mismatches, retargets, by_name, stale_after, chained, 1, excluded);
		return;
	}
	result(sc->tag, legacy, 0, 24, t - t0, mismatches, retargets, by_name, stale_after, chained, 0, excluded);
}

int main(void)
{
	/* A: the observed cg OBS log 6.10.2026 04:04:50 -- the reset finder lists the stale :5961
	 * first, every #1180 verify resolved :5971, the stale record aged out after ~55 s. */
	static const scenario_t observed = {
		"observed",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		55ULL * S_NS, 0,
	};
	/* A2: worst case -- after the first verify every finder lists the stale record first. */
	static const scenario_t stale_first = {
		"stale_first",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		55ULL * S_NS, 0,
	};
	/* B: strih-lx CG-obs shape -- the new :5971 record is listed before the sender delivers on it
	 * (it delivers from 12 s), the stale record lives 60 s. */
	static const scenario_t not_yet = {
		"not_yet_delivering",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		60ULL * S_NS, 12ULL * S_NS,
	};
	/* C: a WRONG retarget -- the first verify resolves a third live sender. */
	static const scenario_t wrong_retarget = {
		"wrong_retarget",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, OTHER }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE }, { NAME, OTHER } }, 2,
		55ULL * S_NS, 0,
	};
	run(&observed, false);
	run(&observed, true);
	run(&stale_first, false);
	run(&not_yet, false);
	run(&wrong_retarget, false);
	return 0;
}
