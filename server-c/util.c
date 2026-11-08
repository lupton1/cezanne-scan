/* Errors, buffers, logging, time, folders and sockets. */
#include "cz.h"
#include "plat.h"

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifndef _WIN32
#include <signal.h>
#include <sys/select.h>
#endif

/* ------------------------------------------------------------------ errors */

static const char *kind_prefix(int kind)
{
    switch (kind) {
    case E_IO: return "I/O error: ";
    case E_PROTOCOL: return "protocol error: ";
    case E_INVALID: return "invalid parameter: ";
    case E_BLOCKED: return "blocked unsafe command: ";
    case E_TIMEOUT: return "timed out: ";
    case E_NOT_FOUND: return "no scanner found: ";
    case E_UNSUPPORTED: return "not supported on this platform: ";
    default: return "";
    }
}

int cz_fail(cz_err *e, int kind, const char *fmt, ...)
{
    va_list ap;
    size_t n;

    if (e == NULL)
        return -1;
    memset(e, 0, sizeof *e);
    e->kind = kind;
    if (kind == E_CANCELLED) {
        snprintf(e->msg, sizeof e->msg, "cancelled");
        return -1;
    }
    snprintf(e->msg, sizeof e->msg, "%s", kind_prefix(kind));
    n = strlen(e->msg);
    va_start(ap, fmt);
    vsnprintf(e->msg + n, sizeof e->msg - n, fmt, ap);
    va_end(ap);
    return -1;
}

/* ------------------------------------------------------------------ memory */

void *xmalloc(size_t n)
{
    void *p = malloc(n ? n : 1);
    if (p == NULL) {
        fprintf(stderr, "out of memory (%lu bytes)\n", (unsigned long)n);
        abort();
    }
    return p;
}

void *xcalloc(size_t n, size_t size)
{
    void *p = calloc(n ? n : 1, size ? size : 1);
    if (p == NULL) {
        fprintf(stderr, "out of memory\n");
        abort();
    }
    return p;
}

void *xrealloc(void *p, size_t n)
{
    void *q = realloc(p, n ? n : 1);
    if (q == NULL) {
        fprintf(stderr, "out of memory (%lu bytes)\n", (unsigned long)n);
        abort();
    }
    return q;
}

char *xstrdup(const char *s)
{
    return xstrndup(s, strlen(s));
}

/* strndup is missing from Mac OS X 10.6 */
char *xstrndup(const char *s, size_t n)
{
    char *p = xmalloc(n + 1);
    memcpy(p, s, n);
    p[n] = 0;
    return p;
}

/* ------------------------------------------------------------------ string buffer */

void sb_init(sbuf *b)
{
    b->p = NULL;
    b->len = b->cap = 0;
}

void sb_free(sbuf *b)
{
    free(b->p);
    sb_init(b);
}

static void sb_reserve(sbuf *b, size_t extra)
{
    if (b->len + extra + 1 > b->cap) {
        size_t cap = b->cap ? b->cap : 256;
        while (cap < b->len + extra + 1)
            cap *= 2;
        b->p = xrealloc(b->p, cap);
        b->cap = cap;
    }
}

void sb_put(sbuf *b, const void *data, size_t n)
{
    sb_reserve(b, n);
    if (n)
        memcpy(b->p + b->len, data, n);
    b->len += n;
    b->p[b->len] = 0;
}

void sb_puts(sbuf *b, const char *s)
{
    sb_put(b, s, strlen(s));
}

void sb_byte(sbuf *b, uint8_t v)
{
    sb_put(b, &v, 1);
}

void sb_printf(sbuf *b, const char *fmt, ...)
{
    va_list ap;
    int n;

    va_start(ap, fmt);
    n = vsnprintf(NULL, 0, fmt, ap);
    va_end(ap);
    if (n < 0)
        return;
    sb_reserve(b, (size_t)n);
    va_start(ap, fmt);
    vsnprintf(b->p + b->len, (size_t)n + 1, fmt, ap);
    va_end(ap);
    b->len += (size_t)n;
}

const char *sb_str(sbuf *b)
{
    if (b->p == NULL)
        sb_reserve(b, 0), b->p[0] = 0;
    return b->p;
}

/* ------------------------------------------------------------------ logging */

void log_line(int error, const char *fmt, ...)
{
    char stamp[32];
    time_t now = time(NULL);
    struct tm tm;
    va_list ap;

#ifdef _WIN32
    localtime_s(&tm, &now);
#else
    localtime_r(&now, &tm);
#endif
    strftime(stamp, sizeof stamp, "%Y-%m-%d %H:%M:%S", &tm);
    fprintf(stderr, "[%s %s] ", stamp, error ? "WARN " : "INFO ");
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
    fflush(stderr);
}

/* ------------------------------------------------------------------ time */

#ifdef _WIN32
double now_seconds(void)
{
    LARGE_INTEGER f, c;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&c);
    return (double)c.QuadPart / (double)f.QuadPart;
}

void sleep_ms(unsigned ms)
{
    Sleep(ms);
}
#else
/* gettimeofday, because clock_gettime only arrived in macOS 10.12 */
double now_seconds(void)
{
    struct timeval tv;
    gettimeofday(&tv, NULL);
    return (double)tv.tv_sec + tv.tv_usec / 1e6;
}

void sleep_ms(unsigned ms)
{
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
    }
}
#endif

uint64_t unix_time(void)
{
    time_t t = time(NULL);
    return t < 0 ? 0 : (uint64_t)t;
}

/* ------------------------------------------------------------------ files */

int file_exists(const char *path)
{
    struct stat st;
    return stat(path, &st) == 0;
}

int make_dirs(const char *path)
{
    char *p = xstrdup(path);
    size_t i, n = strlen(p);
    struct stat st;

    for (i = 1; i <= n; i++) {
        if (p[i] == '/' || p[i] == '\\' || p[i] == 0) {
            char c = p[i];
            p[i] = 0;
            if (stat(p, &st) != 0) {
#ifdef _WIN32
                /* skip a drive letter such as C: */
                if (!(strlen(p) == 2 && p[1] == ':'))
#endif
                    if (cz_mkdir(p) != 0 && errno != EEXIST) {
                        int saved = errno;
                        free(p);
                        errno = saved;
                        return -1;
                    }
            }
            p[i] = c;
        }
    }
    free(p);
    if (stat(path, &st) != 0 || !(st.st_mode & S_IFDIR)) {
        errno = ENOTDIR;
        return -1;
    }
    return 0;
}

/* ------------------------------------------------------------------ sockets */

int sock_init(void)
{
#ifdef _WIN32
    WSADATA w;
    return WSAStartup(MAKEWORD(2, 2), &w) == 0 ? 0 : -1;
#else
    signal(SIGPIPE, SIG_IGN);
    return 0;
#endif
}

void sock_timeouts(sock_t s, unsigned recv_ms, unsigned send_ms)
{
#ifdef _WIN32
    DWORD r = recv_ms, w = send_ms;
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, (const char *)&r, sizeof r);
    setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, (const char *)&w, sizeof w);
#else
    struct timeval r, w;
    r.tv_sec = recv_ms / 1000;
    r.tv_usec = (recv_ms % 1000) * 1000;
    w.tv_sec = send_ms / 1000;
    w.tv_usec = (send_ms % 1000) * 1000;
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &r, sizeof r);
    setsockopt(s, SOL_SOCKET, SO_SNDTIMEO, &w, sizeof w);
#endif
#ifdef SO_NOSIGPIPE
    {
        int one = 1;
        setsockopt(s, SOL_SOCKET, SO_NOSIGPIPE, &one, sizeof one);
    }
#endif
}

void sock_nodelay(sock_t s)
{
    int one = 1;
    setsockopt(s, IPPROTO_TCP, TCP_NODELAY, (const char *)&one, sizeof one);
}

int sock_read_full(sock_t s, void *buf, size_t n)
{
    char *p = buf;
    while (n > 0) {
        int chunk = n > (1u << 30) ? (1 << 30) : (int)n;
        int r = (int)recv(s, p, chunk, 0);
        if (r == 0)
            return -1;
        if (r < 0) {
#ifndef _WIN32
            if (errno == EINTR)
                continue;
#endif
            return -1;
        }
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

int sock_write_full(sock_t s, const void *buf, size_t n)
{
    const char *p = buf;
    while (n > 0) {
        int chunk = n > (1u << 30) ? (1 << 30) : (int)n;
        int r = (int)send(s, p, chunk, 0);
        if (r < 0) {
#ifndef _WIN32
            if (errno == EINTR)
                continue;
#endif
            return -1;
        }
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static int set_blocking(sock_t s, int blocking)
{
#ifdef _WIN32
    u_long nb = blocking ? 0 : 1;
    return ioctlsocket(s, FIONBIO, &nb) == 0 ? 0 : -1;
#else
    int fl = fcntl(s, F_GETFL, 0);
    if (fl < 0)
        return -1;
    return fcntl(s, F_SETFL, blocking ? (fl & ~O_NONBLOCK) : (fl | O_NONBLOCK));
#endif
}

sock_t sock_connect(const char *host, const char *port, unsigned timeout_ms)
{
    struct addrinfo hints, *res, *ai;
    sock_t s = BAD_SOCK;

    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(host, port, &hints, &res) != 0)
        return BAD_SOCK;
    /* like the Rust client, only the first address is tried */
    ai = res;
    if (ai != NULL) {
        s = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (s != BAD_SOCK) {
            int ok = 0;
            set_blocking(s, 0);
            if (connect(s, ai->ai_addr, (int)ai->ai_addrlen) == 0) {
                ok = 1;
            } else {
                fd_set w;
                struct timeval tv;
                FD_ZERO(&w);
                FD_SET(s, &w);
                tv.tv_sec = timeout_ms / 1000;
                tv.tv_usec = (timeout_ms % 1000) * 1000;
                if (select((int)s + 1, NULL, &w, NULL, &tv) == 1) {
                    int err = 0;
                    socklen_t el = sizeof err;
                    if (getsockopt(s, SOL_SOCKET, SO_ERROR, (char *)&err, &el) == 0 && err == 0)
                        ok = 1;
                }
            }
            if (ok) {
                set_blocking(s, 1);
            } else {
                sock_close(s);
                s = BAD_SOCK;
            }
        }
    }
    freeaddrinfo(res);
    return s;
}
