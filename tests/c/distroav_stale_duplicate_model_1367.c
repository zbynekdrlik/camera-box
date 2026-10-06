/* Issue 1367 -- the sequence-replay model of the DistroAV receiver's reset -> bind -> verify loop.
 *
 * NOT compiled on its own: tests/distroav_stale_duplicate_retarget_1367.rs prepends a stub
 * NDIlib_source_t plus the SHIPPED helpers and constants it lifts VERBATIM from
 * vendor/distroav/src/ndi-source.cpp, then compiles the whole under -std=gnu99 -Wall -Wextra
 * -Wformat=2 -Wconversion -Werror and parses the RESULT lines.
 *
 * A scenario is a list of PHASES. Each phase says what every fresh finder lists at a reset, what
 * every #1180 verify finder lists, which URL our sender delivers on (and from when), which URL
 * nobody answers on, and whether the SDK's own BY-NAME resolver reaches our sender. Any other URL
 * a bind lands on belongs to ANOTHER live sender: frames flow, but from the wrong source.
 *
 * The WIRING section below mirrors ndi_source_thread's use of the shipped helpers (Approach 1b). `legacy` runs
 * the pre-1367 wiring (first name match, every mismatch forced BY-NAME, no exclusion); it must
 * reproduce the live ~55 s / 6-mismatch loop, which is what keeps the model honest. */
#define MS_NS 1000000ULL
#define S_NS 1000000000ULL
#define NAME "RESOLUME-SNV (SP-program)"
#define CGOBS "RESOLUME-SNV (cg-obs)"
#define OVERLAY "RESOLUME-SNV (CG OVERLAY)"
#define URL_A "10.77.9.201:5961"
#define URL_B "10.77.9.201:5971"
#define URL_C "10.77.9.201:5981"
#define MAX_REC 6
#define URL_BUF 128

typedef struct {
	uint64_t from_ns; /* phase start, episode-relative */
	NDIlib_source_t reset_list[MAX_REC];
	uint32_t n_reset;
	NDIlib_source_t verify_list[MAX_REC];
	uint32_t n_verify;
	const char *ours;       /* the URL our sender delivers on in this phase */
	uint64_t ours_from_ns;  /* episode-relative: our sender delivers from here */
	const char *dead;       /* a URL nobody answers on (NULL = none) */
	bool by_name_connects;  /* the SDK name resolver reaches our sender */
} phase_t;

typedef struct {
	const char *tag;
	phase_t ph[3];
	unsigned n_ph;
} scenario_t;

static bool same(const char *a, const char *b) { return a && b && strcmp(a, b) == 0; }

static unsigned long long ms(uint64_t ns) { return (unsigned long long)(ns / MS_NS); }

static const phase_t *phase_at(const scenario_t *sc, uint64_t rel)
{
	const phase_t *p = &sc->ph[0];
	for (unsigned i = 1; i < sc->n_ph; ++i)
		if (rel >= sc->ph[i].from_ns)
			p = &sc->ph[i];
	return p;
}

/* The pre-1367 picker: the first record of the name with a usable address. */
static const char *legacy_pick(const NDIlib_source_t *list, uint32_t n)
{
	for (uint32_t i = 0; i < n; ++i)
		if (same(list[i].p_ndi_name, NAME))
			return (list[i].p_url_address && list[i].p_url_address[0]) ? list[i].p_url_address : NULL;
	return NULL;
}

/* ---- WIRING: mirrors ndi_source_thread + its #1367 helpers (Approach 1b, decision 6009040469) ----
 * Every decision is a SHIPPED pure helper: the reset step (ndi_stale_begin_reset_1367), the pick
 * (ndi_find_url_for_source_name with the two slots), the verdict (ndi_identity_verdict_1367) and the
 * action (ndi_stale_apply_verdict_1367). The model only replays the order the thread calls them in. */
#define ACT_KEEP 0
#define ACT_RETARGET 1
#define ACT_BY_NAME 2

typedef struct {
	struct ndi_stale_state_1367 st;
} wiring_t;

static void copy_url(char *dst, const char *src) { snprintf(dst, URL_BUF, "%s", src ? src : ""); }

/* reset block (ndi_stale_reset_1367): expire, consume the retarget. */
static bool wiring_begin_reset(wiring_t *w, uint64_t now, char *retarget_out)
{
	(void)ndi_stale_begin_reset_1367(&w->st, now, retarget_out);
	return retarget_out[0] != '\0';
}

static void wiring_mark_retarget(wiring_t *w) { w->st.bound_via_retarget = true; }

/* the reset's fresh-finder pick: expired slots were cleared by the reset step, as in the thread. */
static const char *wiring_pick(wiring_t *w, const NDIlib_source_t *list, uint32_t n, uint64_t now)
{
	(void)now;
	return ndi_find_url_for_source_name(NAME, list, n, w->st.excluded[0], w->st.excluded[1]);
}

/* the #1180 verify after first frames (ndi_identity_verify_1367); *excluded_out names a URL this
 * verify excluded. */
static int wiring_verify(wiring_t *w, const char *bound, const NDIlib_source_t *list, uint32_t n, uint64_t now,
			 char *excluded_out, bool *mismatch_out)
{
	excluded_out[0] = '\0';
	const char *ex0 = ndi_stale_exclusion_1367(&w->st, 0, now);
	const char *ex1 = ndi_stale_exclusion_1367(&w->st, 1, now);
	int verdict = ndi_identity_verdict_1367(bound, NAME, list, n, ex0, ex1);
	const char *pick =
		verdict == NDI_VERIFY_INCONCLUSIVE_1367 ? NULL : ndi_find_url_for_source_name(NAME, list, n, ex0, ex1);
	int action = ndi_stale_apply_verdict_1367(&w->st, verdict, bound, pick, now);
	*mismatch_out = verdict == NDI_VERIFY_STALE_1367 || verdict == NDI_VERIFY_MISMATCH_1367;
	if (verdict == NDI_VERIFY_STALE_1367)
		copy_url(excluded_out, bound);
	if (action == NDI_STALE_RETARGET_1367)
		return ACT_RETARGET;
	if (action == NDI_STALE_BY_NAME_1367)
		return ACT_BY_NAME;
	return ACT_KEEP;
}

static const char *wiring_retarget(const wiring_t *w) { return w->st.retarget; }

static unsigned wiring_exclusions(const wiring_t *w, uint64_t now)
{
	return (ndi_stale_exclusion_1367(&w->st, 0, now) ? 1u : 0u) + (ndi_stale_exclusion_1367(&w->st, 1, now) ? 1u : 0u);
}
/* ---- END WIRING ---- */

typedef struct {
	unsigned mismatches, retargets, by_name, frameless, wrong_frames, reopened, excluded_ours, max_excl;
} stats_t;

static void result(const char *tag, bool legacy, int attached, unsigned reset, uint64_t rel, const stats_t *st,
		   int wrong_sender)
{
	printf("RESULT %s legacy=%d attached=%d reset=%u t_ms=%llu mismatches=%u retargets=%u by_name_resets=%u frameless_binds=%u wrong_frames=%u reopened=%u excluded_ours=%u max_exclusions=%u wrong_sender=%d\n",
	       tag, legacy ? 1 : 0, attached, reset, ms(rel), st->mismatches, st->retargets, st->by_name, st->frameless,
	       st->wrong_frames, st->reopened, st->excluded_ours, st->max_excl, wrong_sender);
}

static void run(const scenario_t *sc, bool legacy)
{
	const uint64_t t0 = 5ULL * S_NS; /* the monotonic clock is never 0 (0 = "nothing excluded") */
	uint64_t t = t0;
	wiring_t w;
	memset(&w, 0, sizeof w);
	stats_t st;
	memset(&st, 0, sizeof st);
	char ever_excluded[8][URL_BUF];
	unsigned n_ever = 0;
	bool force_by_name = false;
	for (unsigned reset = 1; reset <= 24; ++reset) {
		t += 100ULL * MS_NS; /* the reset block itself */
		uint64_t rel = t - t0;
		const phase_t *ph = phase_at(sc, rel);
		bool forced = force_by_name;
		force_by_name = false;
		char take[URL_BUF];
		take[0] = '\0';
		bool have_retarget = legacy ? false : wiring_begin_reset(&w, t, take);
		const char *bound = NULL;
		const char *mode = "BYNAME";
		if (!forced && have_retarget) {
			bound = take;
			mode = "RETARGET";
			wiring_mark_retarget(&w);
			st.retargets++;
		} else if (!forced) {
			bound = legacy ? legacy_pick(ph->reset_list, ph->n_reset)
				       : wiring_pick(&w, ph->reset_list, ph->n_reset, t);
			mode = bound ? "BYURL" : "BYNAME";
		}
		if (!bound) {
			st.by_name++;
			if (ph->by_name_connects && ph->ours && rel >= ph->ours_from_ns) {
				printf("%s reset %u t_ms=%llu BYNAME -> frames from our sender\n", sc->tag, reset, ms(rel));
				result(sc->tag, legacy, 1, reset, rel, &st, 0);
				return;
			}
			printf("%s reset %u t_ms=%llu BYNAME -> no connection inside the stale window\n", sc->tag, reset,
			       ms(rel));
			t += GENLOCK_RECONNECT_STALE_NS;
			force_by_name = ndi_force_by_name_after_frameless(false, false);
			continue;
		}
		for (unsigned i = 0; i < n_ever; ++i)
			if (same(bound, ever_excluded[i]) && !same(bound, ph->ours))
				st.reopened++;
		bool ours = same(bound, ph->ours);
		bool frameless = same(bound, ph->dead) || (ours && rel < ph->ours_from_ns);
		if (frameless) {
			st.frameless++;
			printf("%s reset %u t_ms=%llu %s %s -> frame-less\n", sc->tag, reset, ms(rel), mode, bound);
			t += GENLOCK_RECONNECT_STALE_NS;
			force_by_name = ndi_force_by_name_after_frameless(true, false);
			continue;
		}
		if (!ours)
			st.wrong_frames++;
		/* frames flow -> the one-shot #1180 identity verify (its own fresh finder) */
		t += 30ULL * MS_NS;
		int action;
		char excluded_now[URL_BUF];
		excluded_now[0] = '\0';
		bool mm = false;
		if (legacy) {
			const char *v = legacy_pick(ph->verify_list, ph->n_verify);
			mm = ndi_by_url_identity_mismatch(bound, v);
			action = mm ? ACT_BY_NAME : ACT_KEEP;
		} else {
			action = wiring_verify(&w, bound, ph->verify_list, ph->n_verify, t, excluded_now, &mm);
		}
		if (excluded_now[0]) {
			if (same(excluded_now, ph->ours))
				st.excluded_ours++;
			if (n_ever < 8)
				copy_url(ever_excluded[n_ever++], excluded_now);
		}
		if (!legacy && wiring_exclusions(&w, t) > st.max_excl)
			st.max_excl = wiring_exclusions(&w, t);
		if (mm)
			st.mismatches++;
		if (action == ACT_RETARGET) {
			printf("%s reset %u t_ms=%llu %s %s -> MISMATCH -> RETARGET %s\n", sc->tag, reset, ms(rel), mode, bound,
			       wiring_retarget(&w));
			continue;
		}
		if (action == ACT_BY_NAME) {
			force_by_name = true;
			printf("%s reset %u t_ms=%llu %s %s -> MISMATCH -> BYNAME\n", sc->tag, reset, ms(rel), mode, bound);
			continue;
		}
		if (ours) {
			printf("%s reset %u t_ms=%llu %s %s -> frames from our sender, kept\n", sc->tag, reset, ms(t - t0), mode,
			       bound);
			result(sc->tag, legacy, 1, reset, t - t0, &st, 0);
			return;
		}
		printf("%s reset %u t_ms=%llu %s %s -> WRONG SENDER ACCEPTED\n", sc->tag, reset, ms(rel), mode, bound);
		result(sc->tag, legacy, 0, reset, t - t0, &st, 1);
		return;
	}
	result(sc->tag, legacy, 0, 24, t - t0, &st, 0);
}

int main(void)
{
	/* The incident ordering (cg OBS 6.10.2026 04:04:50): the stale :5961 record of our name is listed
	 * first, and the sender that now owns :5961 (cg-obs) advertises its own record there. Every verify
	 * resolved :5971. The stale record ages out after ~55 s. */
	static const scenario_t incident = {
		"incident",
		{ { 0,
		    { { NAME, URL_A }, { NAME, URL_B }, { CGOBS, URL_A } }, 3,
		    { { NAME, URL_B }, { NAME, URL_A }, { CGOBS, URL_A } }, 3,
		    URL_B, 0, NULL, false },
		  { 55ULL * S_NS,
		    { { NAME, URL_B }, { CGOBS, URL_A } }, 2,
		    { { NAME, URL_B }, { CGOBS, URL_A } }, 2,
		    URL_B, 0, NULL, true } },
		2,
	};
	/* The reversed ordering (the review's probe): the reset finder lists the live :5971 first, the
	 * verify finder the stale :5961 first, and neither shows :5961's new owner. */
	static const scenario_t reversed = {
		"reversed",
		{ { 0,
		    { { NAME, URL_B }, { NAME, URL_A } }, 2,
		    { { NAME, URL_A }, { NAME, URL_B } }, 2,
		    URL_B, 0, NULL, false },
		  { 55ULL * S_NS,
		    { { NAME, URL_B } }, 1,
		    { { NAME, URL_B } }, 1,
		    URL_B, 0, NULL, true } },
		2,
	};
	/* The old port is dead (nobody owns it) and nothing is contested: today's BY-URL <-> BY-NAME
	 * alternation until the stale record ages out, never a lock-on. */
	static const scenario_t dead_old_port = {
		"dead_old_port",
		{ { 0,
		    { { NAME, URL_A }, { NAME, URL_B } }, 2,
		    { { NAME, URL_A }, { NAME, URL_B } }, 2,
		    URL_B, 0, URL_A, false },
		  { 55ULL * S_NS,
		    { { NAME, URL_B } }, 1,
		    { { NAME, URL_B } }, 1,
		    URL_B, 0, NULL, true } },
		2,
	};
	/* The strih-lx shape: the new :5971 record is listed before the sender delivers on it (from 12 s);
	 * cg-obs owns the old :5961 and advertises it. */
	static const scenario_t still_starting = {
		"still_starting",
		{ { 0,
		    { { NAME, URL_A }, { NAME, URL_B }, { CGOBS, URL_A } }, 3,
		    { { NAME, URL_B }, { NAME, URL_A }, { CGOBS, URL_A } }, 3,
		    URL_B, 12ULL * S_NS, NULL, false },
		  { 60ULL * S_NS,
		    { { NAME, URL_B }, { CGOBS, URL_A } }, 2,
		    { { NAME, URL_B }, { CGOBS, URL_A } }, 2,
		    URL_B, 12ULL * S_NS, NULL, true } },
		2,
	};
	/* Two successive port moves before any bind verified (a crash-looping sender): A -> B, cg-obs
	 * takes A; then B -> C while the first retarget is still connecting, CG OVERLAY takes B. The
	 * reset finder of phase 1 does not show cg-obs yet, and from phase 2 on no finder shows cg-obs
	 * at A any more, so only the first exclusion keeps A from being re-opened. */
	static const scenario_t two_moves = {
		"two_moves",
		{ { 0,
		    { { NAME, URL_A }, { NAME, URL_B } }, 2,
		    { { NAME, URL_B }, { NAME, URL_A }, { CGOBS, URL_A } }, 3,
		    URL_B, 0, NULL, false },
		  { 200ULL * MS_NS,
		    { { NAME, URL_A }, { NAME, URL_B }, { NAME, URL_C }, { OVERLAY, URL_B } }, 4,
		    { { NAME, URL_A }, { NAME, URL_B }, { NAME, URL_C }, { OVERLAY, URL_B } }, 4,
		    URL_C, 0, NULL, false },
		  { 55ULL * S_NS,
		    { { NAME, URL_C }, { OVERLAY, URL_B } }, 2,
		    { { NAME, URL_C }, { OVERLAY, URL_B } }, 2,
		    URL_C, 0, NULL, true } },
		3,
	};
	run(&incident, false);
	run(&incident, true);
	run(&reversed, false);
	run(&dead_old_port, false);
	run(&still_starting, false);
	run(&two_moves, false);
	return 0;
}
