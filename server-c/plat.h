/* Sockets and files on Mac OS X, Linux and (for testing only) Windows. */
#ifndef CZ_PLAT_H
#define CZ_PLAT_H

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <direct.h>
#include <io.h>
typedef SOCKET sock_t;
#define BAD_SOCK INVALID_SOCKET
#define sock_close closesocket
#define cz_mkdir(p) _mkdir(p)
#define PATH_SEP '\\'
#define sock_errno() WSAGetLastError()
#else
#include <arpa/inet.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
typedef int sock_t;
#define BAD_SOCK (-1)
#define sock_close close
#define cz_mkdir(p) mkdir((p), 0755)
#define PATH_SEP '/'
#define sock_errno() errno
#endif

#include <sys/stat.h>
#include <sys/types.h>

/* 0 on success */
int sock_init(void);
void sock_timeouts(sock_t s, unsigned recv_ms, unsigned send_ms);
void sock_nodelay(sock_t s);
/* -1 when the peer closed or an error happened */
int sock_read_full(sock_t s, void *buf, size_t n);
int sock_write_full(sock_t s, const void *buf, size_t n);
/* connect with a time limit, BAD_SOCK on failure */
sock_t sock_connect(const char *host, const char *port, unsigned timeout_ms);

#endif
