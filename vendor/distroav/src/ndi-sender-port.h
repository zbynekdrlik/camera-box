/******************************************************************************
	camera-box issue 1363: keep every NDI sender's TCP port across an OBS restart
	on LINUX.

	libndi (6.3.2) binds each sender listener WITHOUT SO_REUSEADDR and hands
	ports out from an in-process cursor starting at 5961 that never retries a
	port it failed to bind. On Linux a connection left in TIME_WAIT on :5961 by
	the previous OBS (60 s, fixed) therefore makes the next OBS's FIRST sender
	(the reserved program, the #1185 pin) land on :5962 for the whole session,
	and every other sender shifts up by one. Windows binds over a TIME_WAIT,
	which is why the pin always held there.

	Fix (the ROZHODNUTÉ on the ticket, option C):
	  - every sender create goes through ndi_sender_create_tracked(), which
	    records the sender's port (libndi has no API for it: the source URL is
	    NULL for a local sender) as the ONE new TCP listening socket of this
	    process across the send_create, counting only libndi's sender port
	    band (a receiver's own listener opened concurrently is not a sender);
	  - every sender destroy is preceded by
	    ndi_sender_abort_connections_before_destroy(), which sets
	    SO_LINGER {1,0} on that port's CONNECTED sockets AND resets them at
	    once (connect AF_UNSPEC: libndi's destroy shuts a connection down
	    gracefully before it closes it, so linger alone comes too late), so
	    no TIME_WAIT survives;
	  - the :5961 reserve probes the port first and logs one loud WARN-1363
	    line when a TIME_WAIT (a crash/kill skipped the abort) or a live
	    listener holds it. It never waits.

	The pure decision helpers below are std-only and C-compatible so the gate
	test (tests/distroav_sender_port_linger_1363.rs) lifts them verbatim and
	runs a truth table. The impure functions are Linux-only
	(ndi-sender-port.cpp is compiled on Linux only) and every syscall failure
	there is logged and skipped: a shutdown path must never crash OBS.
******************************************************************************/

#pragma once

#include <stddef.h>

// The first TCP port libndi hands a sender.
#define NDI_SENDER_FIRST_TCP_PORT 5961
// libndi's messaging listener (SO_REUSEADDR, never a sender). It is opened by the
// FIRST send_create of a process, so it appears in that create's listen diff.
#define NDI_MESSAGING_TCP_PORT 5960
// libndi's RECEIVER-side TCP listeners start here: a receiver pulling a REMOTE
// source opens its own listener on :6960+ from a libndi thread while it connects
// (measured on dev1 against a dev2 source: :6961, :6962, a transient :6960, :6963
// ...; strih-lx OBS listens on :6961..:6973, one per camera receiver). Those are
// never a sender, and they appear on other threads, so the sender-create mutex
// cannot keep them out of a create's diff.
// The sender band is [NDI_SENDER_FIRST_TCP_PORT, NDI_RECEIVER_FIRST_TCP_PORT).
#define NDI_RECEIVER_FIRST_TCP_PORT 6960
// ndi_new_listen_port(): more than one new sender-band listener appeared.
#define NDI_LISTEN_PORT_AMBIGUOUS (-1)

// Who holds the first sender port right before the program reservation.
enum ndi_first_port_state {
	NDI_FIRST_PORT_FREE = 0,
	NDI_FIRST_PORT_TIME_WAIT = 1,
	NDI_FIRST_PORT_LIVE_LISTENER = 2,
	NDI_FIRST_PORT_UNKNOWN = 3
};

// Classify the two bind probes: a plain (no SO_REUSEADDR) bind on 0.0.0.0:port,
// then, only when that fails with EADDRINUSE, a plain bind on 127.0.0.2:port.
// The alias bind succeeds over TIME_WAIT/closing connections (their local
// address is 127.0.0.1 or the LAN IP) and fails over a live 0.0.0.0 listener.
// A SO_REUSEADDR probe cannot tell the two apart on Linux (libndi's TIME_WAIT
// socket lacks the flag). Any other errno is UNKNOWN.
static inline int ndi_first_port_hold_state(int plain_bind_ok, int plain_addr_in_use, int alias_bind_ok,
					    int alias_addr_in_use)
{
	if (plain_bind_ok)
		return NDI_FIRST_PORT_FREE;
	if (!plain_addr_in_use)
		return NDI_FIRST_PORT_UNKNOWN;
	if (alias_bind_ok)
		return NDI_FIRST_PORT_TIME_WAIT;
	if (alias_addr_in_use)
		return NDI_FIRST_PORT_LIVE_LISTENER;
	return NDI_FIRST_PORT_UNKNOWN;
}

// Human text for the WARN-1363 line.
static inline const char *ndi_first_port_hold_state_text(int state)
{
	switch (state) {
	case NDI_FIRST_PORT_FREE:
		return "free when probed (another sender bound it first)";
	case NDI_FIRST_PORT_TIME_WAIT:
		return "held by a TIME_WAIT from the previous OBS (a crash or kill skipped the stop-time abort)";
	case NDI_FIRST_PORT_LIVE_LISTENER:
		return "held by a live listener (another NDI sender process on this box)";
	case NDI_FIRST_PORT_UNKNOWN:
		return "not classifiable (the bind probe failed for another reason)";
	default:
		return "in an unknown state";
	}
}

// Log WARN-1363 after the reserve? landed_port is the port the reserved sender
// actually got (0 = could not be identified; the tracked create already warned).
// Silent when the pin held. Loud when it landed elsewhere, or when the port is
// unknown but the probe said :5961 was held.
static inline int ndi_reserve_should_warn(int hold_state, int landed_port)
{
	if (landed_port == NDI_SENDER_FIRST_TCP_PORT)
		return 0;
	if (landed_port > 0)
		return 1;
	return hold_state != NDI_FIRST_PORT_FREE;
}

// Is this TCP socket one of the sender's accepted connections? Only a
// connected, non-listening socket whose LOCAL port is the sender's port. The
// listener never enters TIME_WAIT; the client side of an in-process receiver
// has an ephemeral local port; an unknown sender port (0) touches nothing.
static inline int ndi_socket_is_sender_connection(int is_listener, int has_peer, int local_port, int sender_port)
{
	return !is_listener && has_peer && sender_port > 0 && local_port == sender_port;
}

// Is `port` in libndi's sender port band? Excludes the :5960 messaging listener
// (below the band), the receivers' :6960+ listeners, obs-websocket (:4455) and
// every ephemeral port.
static inline int ndi_is_sender_band_port(int port)
{
	return port >= NDI_SENDER_FIRST_TCP_PORT && port < NDI_RECEIVER_FIRST_TCP_PORT;
}

// The sender's port: the ONE sender-band port listening after its send_create
// that was not listening before. Listeners outside the band (libndi's :5960
// messaging socket that the first send_create of a process opens, a receiver's
// :6960+ listener opened by another thread meanwhile, any non-NDI listener) are
// ignored. 0 when no new sender-band port appeared, NDI_LISTEN_PORT_AMBIGUOUS
// when two different ones did (never a guess; a port seen twice, e.g. on IPv4
// and IPv6, counts once).
static inline int ndi_new_listen_port(const int *before, size_t n_before, const int *after, size_t n_after)
{
	int found = 0;
	for (size_t i = 0; i < n_after; i++) {
		const int p = after[i];
		if (!ndi_is_sender_band_port(p))
			continue;
		int seen = 0;
		for (size_t j = 0; j < n_before; j++) {
			if (before[j] == p) {
				seen = 1;
				break;
			}
		}
		if (seen)
			continue;
		if (found == 0)
			found = p;
		else if (found != p)
			return NDI_LISTEN_PORT_AMBIGUOUS;
	}
	return found;
}

// Reason text for the PORTID-1363 line when a sender's port stays unknown.
// snapshots_ok = both /proc/self/fd listener snapshots were read; result = the
// ndi_new_listen_port() value (0 or NDI_LISTEN_PORT_AMBIGUOUS).
static inline const char *ndi_listen_port_failure_text(int snapshots_ok, int result)
{
	if (!snapshots_ok)
		return "/proc/self/fd unreadable";
	if (result == NDI_LISTEN_PORT_AMBIGUOUS)
		return "more than one new listener in the sender band";
	return "no new listener in the sender band";
}

#ifdef __linux__
#include "plugin-main.h"

// send_create + record the new sender's TCP listen port in *out_port (0 when it
// cannot be identified; logged). All tracked creates are serialized.
NDIlib_send_instance_t ndi_sender_create_tracked(const NDIlib_send_create_t *desc, int *out_port);

// Right before send_destroy: on every connected socket whose local port is
// `port`, SO_LINGER {1,0} plus an immediate reset (connect AF_UNSPEC), so no
// TIME_WAIT is left even though libndi shuts the socket down before closing it.
// `name` is only a log label (an output's NDI name, a filter's source name;
// the port ties it to the create line). port <= 0 is a no-op. Best-effort:
// every failure is logged, never fatal.
void ndi_sender_abort_connections_before_destroy(int port, const char *name);

// Probe who holds TCP `port` right now (one of enum ndi_first_port_state).
int ndi_sender_port_hold_state(int port);
#endif
