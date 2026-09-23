/******************************************************************************
	camera-box issue 1363: Linux NDI sender-port hygiene (see ndi-sender-port.h
	for the root cause and the decision). Linux only: Windows binds a sender
	listener over a TIME_WAIT, so nothing here is needed there, and this whole
	translation unit compiles to nothing on other platforms (CMakeLists.txt also
	adds it on Linux only).

	Known, accepted race: the fd scan runs on sockets libndi owns. If a libndi
	thread closes an fd and the number is reused between our getsockname() and
	the abort (setsockopt + disconnect), the worst case is one unrelated socket
	reset instead of closed with a FIN. Every syscall failure is logged and
	skipped. A libndi thread that writes to a connection after the abort gets
	EPIPE, never a signal: the OBS frontend blocks SIGPIPE in every thread.

	Not covered (a TIME_WAIT can still form, harmful only on a relaunch within
	60 s): a connection libndi itself closes DURING the session (not at
	destroy). A crash/kill never runs the abort at all; the :5961 reserve
	reports that case as WARN-1363.
******************************************************************************/

#ifdef __linux__

#include "ndi-sender-port.h"

#include <arpa/inet.h>
#include <cerrno>
#include <climits>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <dirent.h>
#include <mutex>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
#include <vector>

// Serializes every tracked send_create, so the listen-port diff of one create
// is never polluted by another sender being created on a different thread.
// Held only around the two snapshots and libndi's send_create (no plugin lock
// and no obs_log inside), so it is always the innermost plugin lock.
static std::mutex g_ndi_sender_create_mutex;

// Local TCP port of a socket fd, or 0 when it is not an IPv4/IPv6 socket.
static int ndi_socket_local_port(int fd)
{
	struct sockaddr_storage local;
	socklen_t len = sizeof(local);
	memset(&local, 0, sizeof(local));
	if (getsockname(fd, (struct sockaddr *)&local, &len) != 0)
		return 0;
	if (local.ss_family == AF_INET)
		return ntohs(((struct sockaddr_in *)&local)->sin_port);
	if (local.ss_family == AF_INET6)
		return ntohs(((struct sockaddr_in6 *)&local)->sin6_port);
	return 0;
}

static int ndi_socket_is_listening(int fd)
{
	int acc = 0;
	socklen_t len = sizeof(acc);
	return (getsockopt(fd, SOL_SOCKET, SO_ACCEPTCONN, &acc, &len) == 0 && acc) ? 1 : 0;
}

static int ndi_socket_has_peer(int fd)
{
	struct sockaddr_storage peer;
	socklen_t len = sizeof(peer);
	return getpeername(fd, (struct sockaddr *)&peer, &len) == 0 ? 1 : 0;
}

// Call fn(fd, local_port) for every TCP (SOCK_STREAM, IPv4/IPv6) socket fd of
// this process. Returns false (errno set) when /proc/self/fd cannot be read.
template<typename F> static bool ndi_for_each_tcp_socket(F fn)
{
	DIR *dir = opendir("/proc/self/fd");
	if (!dir)
		return false;
	const int dir_fd = dirfd(dir);
	struct dirent *entry;
	while ((entry = readdir(dir)) != nullptr) {
		char *end = nullptr;
		const long value = strtol(entry->d_name, &end, 10);
		if (end == entry->d_name || *end != '\0' || value < 0 || value > INT_MAX)
			continue; // ".", ".."
		const int fd = (int)value;
		if (fd == dir_fd)
			continue;
		int type = 0;
		socklen_t len = sizeof(type);
		if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &len) != 0 || type != SOCK_STREAM)
			continue; // not a socket, or not TCP
		const int port = ndi_socket_local_port(fd);
		if (port <= 0)
			continue;
		fn(fd, port);
	}
	closedir(dir);
	return true;
}

static bool ndi_listen_ports_snapshot(std::vector<int> &ports)
{
	ports.clear();
	return ndi_for_each_tcp_socket([&](int fd, int port) {
		if (ndi_socket_is_listening(fd))
			ports.push_back(port);
	});
}

NDIlib_send_instance_t ndi_sender_create_tracked(const NDIlib_send_create_t *desc, int *out_port)
{
	if (out_port)
		*out_port = 0;
	const char *name = (desc && desc->p_ndi_name) ? desc->p_ndi_name : "";

	std::vector<int> before;
	std::vector<int> after;
	bool have_before = false;
	bool have_after = false;
	NDIlib_send_instance_t sender = nullptr;
	{
		std::lock_guard<std::mutex> lock(g_ndi_sender_create_mutex);
		have_before = ndi_listen_ports_snapshot(before);
		sender = ndiLib->send_create(desc);
		if (sender)
			have_after = ndi_listen_ports_snapshot(after);
	}
	if (!sender)
		return sender; // the caller logs the create failure

	int found = 0;
	if (have_before && have_after)
		found = ndi_new_listen_port(before.data(), before.size(), after.data(), after.size());
	const int port = found > 0 ? found : 0;
	if (port > 0) {
		obs_log(LOG_INFO, "ndi-sender-port: NDI sender '%s' listens on TCP :%d (#1363)", name, port);
	} else {
		// Its own label: WARN-1363 is reserved for the :5961 reserve line.
		obs_log(LOG_WARNING,
			"PORTID-1363 - ndi-sender-port: could not identify the TCP port of NDI sender '%s' (%s) - "
			"libndi sender port band :%d-:%d; its connections will close normally at stop, so a relaunch "
			"within 60 s may shift its port",
			name, ndi_listen_port_failure_text(have_before && have_after, found), NDI_SENDER_FIRST_TCP_PORT,
			NDI_RECEIVER_FIRST_TCP_PORT - 1);
	}
	if (out_port)
		*out_port = port;
	return sender;
}

void ndi_sender_abort_connections_before_destroy(int port, const char *name)
{
	if (port <= 0)
		return; // unknown port: nothing to target (the create already warned)
	const char *who = name ? name : "";
	int aborted = 0;
	int reset = 0;
	int failed = 0;
	const bool scanned = ndi_for_each_tcp_socket([&](int fd, int local_port) {
		const int is_listener = ndi_socket_is_listening(fd);
		const int has_peer = ndi_socket_has_peer(fd);
		if (!ndi_socket_is_sender_connection(is_listener, has_peer, local_port, port))
			return;
		struct linger lg;
		lg.l_onoff = 1;
		lg.l_linger = 0;
		const bool lingered = setsockopt(fd, SOL_SOCKET, SO_LINGER, &lg, sizeof(lg)) == 0;
		if (lingered)
			aborted++;
		// Reset NOW, not at libndi's close(): libndi's send_destroy calls
		// shutdown(SHUT_RDWR) first, which sends a graceful FIN whatever SO_LINGER
		// says, and a viewer's answering FIN can arrive before the close() (the
		// socket is then already in TIME_WAIT). A connect() with AF_UNSPEC is the
		// kernel's tcp_disconnect: RST now, socket to CLOSE, fd left open for libndi,
		// whose shutdown()/close() then send nothing. SO_LINGER 0 stays as the
		// fallback when the disconnect fails.
		struct sockaddr unspec;
		memset(&unspec, 0, sizeof(unspec));
		unspec.sa_family = AF_UNSPEC;
		const bool disconnected = connect(fd, &unspec, sizeof(unspec)) == 0;
		if (disconnected)
			reset++;
		if (!lingered || !disconnected)
			failed++;
	});
	if (!scanned) {
		obs_log(LOG_WARNING,
			"LINGER-1363 - ndi-sender-port: cannot read /proc/self/fd (errno %d) before destroying the NDI "
			"sender on TCP :%d ('%s'); its connections close normally, so a relaunch within 60 s may shift "
			"its port",
			errno, port, who);
		return;
	}
	obs_log(failed ? LOG_WARNING : LOG_INFO,
		"ndi-sender-port: TCP :%d ('%s'): %d connection(s) set to close with RST (SO_LINGER 0), %d reset at "
		"once (AF_UNSPEC disconnect), %d failed (#1363)",
		port, who, aborted, reset, failed);
}

// errno of a plain (no SO_REUSEADDR) bind on addr:port, 0 when it binds. The
// probe socket is closed at once; a bound but never-connected socket leaves no
// TIME_WAIT.
static int ndi_plain_bind_errno(uint32_t addr_host_order, int port)
{
	const int fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (fd < 0)
		return errno ? errno : -1;
	struct sockaddr_in addr;
	memset(&addr, 0, sizeof(addr));
	addr.sin_family = AF_INET;
	addr.sin_port = htons((uint16_t)port);
	addr.sin_addr.s_addr = htonl(addr_host_order);
	int err = 0;
	if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0)
		err = errno ? errno : -1;
	close(fd);
	return err;
}

int ndi_sender_port_hold_state(int port)
{
	if (port <= 0 || port > 65535)
		return NDI_FIRST_PORT_UNKNOWN;
	const int plain = ndi_plain_bind_errno(INADDR_ANY, port);
	int alias = -1;
	if (plain == EADDRINUSE)
		alias = ndi_plain_bind_errno(0x7f000002u /* 127.0.0.2 */, port);
	return ndi_first_port_hold_state(plain == 0, plain == EADDRINUSE, alias == 0, alias == EADDRINUSE);
}

#endif // __linux__
