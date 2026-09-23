// issue 1363 gate: the real ndi-sender-port.cpp over real loopback sockets.
#include "plugin-main.h"
#include "ndi-sender-port.h"
#include <arpa/inet.h>
#include <cstdio>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

struct fake_sender {
	int listen_fd;
	int conn_fd;
	int extra_fd;  // a concurrent libndi RECEIVER listener (:6961, else ephemeral), or -1
	int extra2_fd; // a second sender-band listener (the ambiguous case), or -1
	int peer_fd;   // the viewer's end of conn_fd (it closes as soon as it sees EOF), or -1
};

// What else opens a listener while this create runs: 0 nothing, 1 a receiver listener outside
// the sender band (the live strih-lx race), 2 a second sender-band listener (ambiguous).
static int g_extra_mode = 0;

// A 0.0.0.0 listener WITHOUT SO_REUSEADDR on `port` (0 = ephemeral). -1 on failure.
static int plain_listener(int port)
{
	int fd = socket(AF_INET, SOCK_STREAM, 0);
	if (fd < 0)
		return -1;
	sockaddr_in a{};
	a.sin_family = AF_INET;
	a.sin_port = htons((uint16_t)port);
	a.sin_addr.s_addr = htonl(INADDR_ANY);
	if (bind(fd, (sockaddr *)&a, sizeof a) != 0 || listen(fd, 4) != 0) {
		close(fd);
		return -1;
	}
	return fd;
}

// Like libndi: the first free port of the sender band, walking up from 5961 on a failed bind.
static int band_listener(void)
{
	for (int p = 5961; p < 6960; p++) {
		const int fd = plain_listener(p);
		if (fd >= 0)
			return fd;
	}
	return -1;
}

static NDIlib_send_instance_t fake_send_create(const NDIlib_send_create_t *)
{
	const int fd = band_listener();
	if (fd < 0)
		return nullptr;
	// A receiver's listener: :6961 like libndi's, an ephemeral port only if :6961 is busy.
	int extra = -1;
	if (g_extra_mode == 1) {
		extra = plain_listener(6961);
		if (extra < 0)
			extra = plain_listener(0);
	}
	const int extra2 = g_extra_mode == 2 ? band_listener() : -1;
	return (NDIlib_send_instance_t) new fake_sender{fd, -1, extra, extra2, -1};
}

// Like libndi 6.3.2 (strace of a real stop): shutdown(SHUT_RDWR) on the accepted connection,
// then close() it. A viewer answers the FIN with its own at once, so by the time of the close()
// the socket is already in TIME_WAIT unless it was reset before the shutdown.
static void fake_send_destroy(NDIlib_send_instance_t p)
{
	auto *s = (fake_sender *)p;
	if (!s)
		return;
	if (s->conn_fd >= 0) {
		shutdown(s->conn_fd, SHUT_RDWR);
		if (s->peer_fd >= 0)
			close(s->peer_fd); // the viewer closes on EOF -> its FIN comes back
		usleep(100000);
		close(s->conn_fd);
	}
	if (s->extra_fd >= 0)
		close(s->extra_fd);
	if (s->extra2_fd >= 0)
		close(s->extra2_fd);
	close(s->listen_fd);
	delete s;
}

static const NDIlib_v6 fake_lib = {fake_send_create, fake_send_destroy};
const NDIlib_v6 *ndiLib = &fake_lib;

static int scenario(const char *label, bool abort_first)
{
	NDIlib_send_create_t desc{};
	desc.p_ndi_name = label;
	int port = -1;
	NDIlib_send_instance_t s = ndi_sender_create_tracked(&desc, &port);
	if (!s || port <= 0) {
		printf("%s.port_found=0\n", label);
		return 1;
	}
	printf("%s.port_found=1\n", label);
	auto *fs = (fake_sender *)s;
	int c = socket(AF_INET, SOCK_STREAM, 0);
	sockaddr_in a{};
	a.sin_family = AF_INET;
	a.sin_port = htons((uint16_t)port);
	a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	if (c < 0 || connect(c, (sockaddr *)&a, sizeof a) != 0)
		return 2;
	fs->conn_fd = accept(fs->listen_fd, nullptr, nullptr);
	if (fs->conn_fd < 0)
		return 3;
	char b = 'x';
	if (write(c, &b, 1) != 1 || read(fs->conn_fd, &b, 1) != 1)
		return 4;
	fs->peer_fd = c; // the fake destroy closes it, like a viewer reacting to the FIN
	printf("%s.live_state=%d\n", label, ndi_sender_port_hold_state(port));
	if (abort_first)
		ndi_sender_abort_connections_before_destroy(port, label);
	ndiLib->send_destroy(s); // the sender shuts down FIRST (active close), as OBS does at stop
	usleep(100000);
	printf("%s.after_state=%d\n", label, ndi_sender_port_hold_state(port));
	return 0;
}

int main()
{
	// An unknown port must be a harmless no-op, never a crash.
	ndi_sender_abort_connections_before_destroy(0, "unknown");
	int rc = scenario("control", false);
	if (rc == 0)
		rc = scenario("linger", true);
	// The live strih-lx race: a libndi receiver opens its listener during the create.
	g_extra_mode = 1;
	if (rc == 0)
		rc = scenario("race", true);
	// Two sender-band listeners in one create: unidentifiable, its own label, no crash.
	g_extra_mode = 2;
	if (rc == 0) {
		NDIlib_send_create_t desc{};
		desc.p_ndi_name = "ambiguous";
		int port = -1;
		NDIlib_send_instance_t s = ndi_sender_create_tracked(&desc, &port);
		printf("ambiguous.created=%d\n", s ? 1 : 0);
		printf("ambiguous.port=%d\n", port);
		ndi_sender_abort_connections_before_destroy(port, "ambiguous");
		if (s)
			ndiLib->send_destroy(s);
	}
	fflush(stdout);
	return rc;
}
