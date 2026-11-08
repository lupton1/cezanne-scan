/*
 * cezanne-server in C: the web server, its JSON API and the scanner thread.
 *
 * A port of crates/cezanne-server (main.rs, state.rs, api.rs) for Mac OS X 10.6 and later.
 * It serves the same web page (crates/cezanne-server/web, built into the program) and
 * answers the same API, so the page works unchanged.
 *
 * Threads: one per browser connection, plus one scanner thread that owns the scanner and
 * takes requests from a queue. Shared state is guarded by one mutex.
 */
#include "cz.h"
#include "plat.h"
#include "assets.h"

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <math.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

#define LOG_LINES 500
#define MAX_BODY (16u * 1024u * 1024u)
#define MAX_HEAD (64u * 1024u)
#define MAX_CONNECTIONS 48

/* ------------------------------------------------------------------ state */

typedef struct {
    uint64_t id;
    char *file_name, *frame, *path;
    uint64_t bytes;
    uint32_t width, height;
    int channels, bits;
    uint64_t created;
} scan_file;

typedef struct {
    uint64_t seq;
    char *text;
    int error;
} log_entry;

typedef struct {
    int refs;
    cz_raw *raw;
    sbuf png;
    double bed_w, bed_h;
} prescan;

typedef struct {
    char *name;
    cz_window window;
    cz_adjust adj;
    int auto_levels;
    char *path;
    int sixteen;
} frame_job;

enum { CONN_SIM, CONN_AUTO, CONN_LOCAL, CONN_BRIDGE };
enum { REQ_CONNECT, REQ_DISCONNECT, REQ_PROBE, REQ_PRESCAN, REQ_SCAN };

typedef struct request request;
struct request {
    int kind;
    int conn;
    char *address, *token; /* CONNECT */
    cz_window window;      /* PRESCAN */
    frame_job *jobs;       /* SCAN */
    int njobs;
    request *next;
};

static struct {
    /* config */
    char *output_dir;
    char *token; /* NULL: no token */
    const char *start_kind;
    char host[256];

    pthread_mutex_t lock;
    pthread_cond_t wake;
    request *queue_head, *queue_tail;
    volatile int cancel;

    /* shared, under lock */
    char *connected;
    int busy;
    char task[300];
    float progress;
    log_entry log[LOG_LINES];
    int log_start, log_count;
    uint64_t log_seq;
    prescan *pre;
    uint64_t prescan_seq;
    scan_file *files;
    size_t nfiles, files_cap;
    uint64_t file_seq;
    int connections;
} S;

static const cz_limits *LIMITS = &LIMITS_FT_S5000;

static void lock(void) { pthread_mutex_lock(&S.lock); }
static void unlock(void) { pthread_mutex_unlock(&S.lock); }

/* add a line to the page's log, the lock must be held */
static void slog_locked(int error, const char *fmt, ...)
{
    char *text;
    va_list ap;
    int n;
    log_entry *slot;

    va_start(ap, fmt);
    n = vsnprintf(NULL, 0, fmt, ap);
    va_end(ap);
    text = xmalloc((size_t)n + 1);
    va_start(ap, fmt);
    vsnprintf(text, (size_t)n + 1, fmt, ap);
    va_end(ap);
    log_line(error, "%s", text);
    S.log_seq++;
    if (S.log_count == LOG_LINES) {
        free(S.log[S.log_start].text);
        S.log_start = (S.log_start + 1) % LOG_LINES;
        S.log_count--;
    }
    slot = &S.log[(S.log_start + S.log_count) % LOG_LINES];
    slot->seq = S.log_seq;
    slot->text = text;
    slot->error = error;
    S.log_count++;
}

#define slog(error, ...) (lock(), slog_locked((error), __VA_ARGS__), unlock())

static prescan *prescan_ref(void)
{
    prescan *p;
    lock();
    p = S.pre;
    if (p != NULL)
        p->refs++;
    unlock();
    return p;
}

static void prescan_unref(prescan *p)
{
    int last;
    if (p == NULL)
        return;
    lock();
    last = --p->refs == 0;
    unlock();
    if (last) {
        raw_free(p->raw);
        sb_free(&p->png);
        free(p);
    }
}

static void file_free(scan_file *f)
{
    free(f->file_name);
    free(f->frame);
    free(f->path);
}

static void push_file_locked(scan_file *f)
{
    if (S.nfiles == S.files_cap) {
        S.files_cap = S.files_cap ? S.files_cap * 2 : 16;
        S.files = xrealloc(S.files, S.files_cap * sizeof *S.files);
    }
    S.files[S.nfiles++] = *f;
}

static char *join_path(const char *dir, const char *name)
{
    size_t n = strlen(dir);
    char *p = xmalloc(n + strlen(name) + 2);
    if (n > 0 && (dir[n - 1] == '/' || dir[n - 1] == '\\'))
        sprintf(p, "%s%s", dir, name);
    else
        sprintf(p, "%s%c%s", dir, PATH_SEP, name);
    return p;
}

static const char *base_name(const char *path)
{
    const char *a = strrchr(path, '/'), *b = strrchr(path, '\\');
    const char *s = a > b ? a : b;
    return s ? s + 1 : path;
}

/* ------------------------------------------------------------------ earlier scans */

typedef struct {
    uint64_t t;
    char *path;
} found_file;

static int found_cmp(const void *a, const void *b)
{
    const found_file *x = a, *y = b;
    if (x->t != y->t)
        return x->t < y->t ? -1 : 1;
    return strcmp(x->path, y->path);
}

static int has_tiff_ext(const char *name)
{
    const char *dot = strrchr(name, '.');
    char ext[8];
    size_t i;
    if (dot == NULL || strlen(dot + 1) >= sizeof ext)
        return 0;
    for (i = 0; dot[1 + i]; i++)
        ext[i] = (char)tolower((unsigned char)dot[1 + i]);
    ext[i] = 0;
    return strcmp(ext, "tif") == 0 || strcmp(ext, "tiff") == 0;
}

/* list the scans already in the output folder so files from before a restart can still
 * be downloaded */
static void load_existing_files(void)
{
    DIR *d = opendir(S.output_dir);
    struct dirent *de;
    found_file *found = NULL;
    size_t n = 0, cap = 0, i;

    if (d == NULL)
        return;
    while ((de = readdir(d)) != NULL) {
        struct stat st;
        char *p;
        if (!has_tiff_ext(de->d_name))
            continue;
        p = join_path(S.output_dir, de->d_name);
        if (stat(p, &st) != 0 || !(st.st_mode & S_IFREG)) {
            free(p);
            continue;
        }
        if (n == cap) {
            cap = cap ? cap * 2 : 32;
            found = xrealloc(found, cap * sizeof *found);
        }
        found[n].t = st.st_mtime < 0 ? 0 : (uint64_t)st.st_mtime;
        found[n].path = p;
        n++;
    }
    closedir(d);
    qsort(found, n, sizeof *found, found_cmp);
    lock();
    for (i = 0; i < n; i++) {
        uint32_t w, h;
        uint16_t bps, spp;
        struct stat st;
        scan_file f;
        if (tiff_read_info(found[i].path, &w, &h, &bps, &spp) < 0) {
            free(found[i].path);
            continue;
        }
        memset(&f, 0, sizeof f);
        f.id = ++S.file_seq;
        f.frame = xstrdup("");
        f.bytes = stat(found[i].path, &st) == 0 ? (uint64_t)st.st_size : 0;
        f.file_name = xstrdup(base_name(found[i].path));
        f.path = found[i].path;
        f.width = w;
        f.height = h;
        f.channels = spp;
        f.bits = bps;
        f.created = found[i].t;
        push_file_locked(&f);
    }
    if (S.nfiles > 0)
        slog_locked(0, "%lu earlier scans found in %s", (unsigned long)S.nfiles, S.output_dir);
    unlock();
    free(found);
}

/* next free prefixNNNN.tif in the output folder, not already given to this request */
static char *next_path(const char *prefix, char **used, int nused)
{
    char name[128];
    unsigned n;
    int i;
    for (n = 1; n < 1000000; n++) {
        char *p;
        int taken = 0;
        snprintf(name, sizeof name, "%s%04u.tif", prefix, n);
        p = join_path(S.output_dir, name);
        for (i = 0; i < nused; i++)
            if (strcmp(used[i], p) == 0)
                taken = 1;
        if (!taken && !file_exists(p))
            return p;
        free(p);
    }
    snprintf(name, sizeof name, "%soverflow.tif", prefix);
    return join_path(S.output_dir, name);
}

/* ------------------------------------------------------------------ the scanner thread */

static void request_free(request *r)
{
    int i;
    free(r->address);
    free(r->token);
    for (i = 0; i < r->njobs; i++) {
        free(r->jobs[i].name);
        free(r->jobs[i].path);
    }
    free(r->jobs);
    free(r);
}

/* hand a request to the scanner thread. Prescans and scans mark the server busy so a
 * second browser cannot start another at the same time. NULL or the reason it was refused. */
static const char *send_request(request *r, char *why, size_t whylen)
{
    lock();
    if (r->kind == REQ_PRESCAN || r->kind == REQ_SCAN) {
        if (S.busy) {
            snprintf(why, whylen, "The scanner is busy (%s)", S.task);
            unlock();
            request_free(r);
            return why;
        }
        if (S.connected == NULL) {
            snprintf(why, whylen, "Not connected to a scanner");
            unlock();
            request_free(r);
            return why;
        }
        S.busy = 1;
        S.progress = 0.0f;
        snprintf(S.task, sizeof S.task, "%s", r->kind == REQ_PRESCAN ? "Prescan" : "Scan");
    }
    r->next = NULL;
    if (S.queue_tail != NULL)
        S.queue_tail->next = r;
    else
        S.queue_head = r;
    S.queue_tail = r;
    pthread_cond_signal(&S.wake);
    unlock();
    return NULL;
}

static void conn_label(const request *r, char *out, size_t size)
{
    switch (r->conn) {
    case CONN_SIM: snprintf(out, size, "Simulator"); break;
    case CONN_AUTO: snprintf(out, size, "This computer (automatic)"); break;
    case CONN_LOCAL: snprintf(out, size, "This computer, %s", r->address); break;
    default: snprintf(out, size, "Bridge %s", r->address); break;
    }
}

static void trim_in_place(char *s)
{
    size_t n = strlen(s), i = 0;
    while (n > 0 && isspace((unsigned char)s[n - 1]))
        s[--n] = 0;
    while (s[i] && isspace((unsigned char)s[i]))
        i++;
    if (i)
        memmove(s, s + i, n - i + 1);
}

static cz_transport *open_connection(const request *r, cz_err *e)
{
    switch (r->conn) {
    case CONN_SIM:
        cz_fail(e, E_UNSUPPORTED,
                "the simulator is not part of this build. Run cezanne-bridge --sim on another computer and connect "
                "to it as a Bridge");
        return NULL;
    case CONN_AUTO: {
        cz_device d[8];
        int n = cz_discover(d, 8, e);
        if (n < 0)
            return NULL;
        if (n == 0) {
            cz_fail(e, E_NOT_FOUND, "no SCREEN scanner found on the SCSI bus");
            return NULL;
        }
        log_info("using %s %s at %s", d[0].vendor, d[0].product, d[0].address);
        return cz_open_local(d[0].address, e);
    }
    case CONN_LOCAL:
        return cz_open_local(r->address, e);
    default: {
        char *host = xstrdup(r->address);
        cz_transport *t;
        trim_in_place(host);
        if (host[0] == 0) {
            free(host);
            cz_fail(e, E_INVALID, "enter the bridge host name or address");
            return NULL;
        }
        t = cz_tcp_connect(host, r->token, e);
        free(host);
        return t;
    }
    }
}

static void on_progress(void *ctx, float p)
{
    (void)ctx;
    lock();
    S.progress = p;
    unlock();
}

/* linear scanner data to an sRGB-ish PNG for the bed view (state.rs display_png) */
static void display_png(const cz_raw *img, sbuf *out)
{
    uint8_t lut[4096];
    size_t i, n = (size_t)img->width * img->height;
    uint8_t *rgba = xmalloc(n * 4);
    int k;
    for (k = 0; k < 4096; k++) {
        double v = round(pow((double)k / 4095.0, 1.0 / 2.2) * 255.0);
        lut[k] = v <= 0 ? 0 : v >= 255 ? 255 : (uint8_t)v;
    }
    for (i = 0; i < n; i++) {
        const uint16_t *px = img->data + i * (size_t)img->channels;
        int c;
        for (c = 0; c < 3; c++)
            rgba[i * 4 + (size_t)c] = lut[px[c < img->channels - 1 ? c : img->channels - 1] >> 4];
        rgba[i * 4 + 3] = 255;
    }
    png_encode_rgba(img->width, img->height, rgba, out);
    free(rgba);
}

static int scan_one(cz_scanner *s, const frame_job *job, scan_file *out, cz_err *e)
{
    cz_raw *raw = scanner_scan(s, &job->window, on_progress, NULL, &S.cancel, e);
    cz_adjust adj;
    cz_processed *img;
    sbuf icc;
    struct stat st;
    char *dir;

    if (raw == NULL)
        return -1;
    adj = job->adj;
    if (job->auto_levels)
        adjust_auto_levels(&adj, raw, 0.002);
    img = adjust_apply(&adj, raw);
    raw_free(raw);
    sb_init(&icc);
    icc_output_profile(img->channels, &icc);
    dir = xstrdup(job->path);
    {
        char *slash = strrchr(dir, PATH_SEP);
        if (slash != NULL) {
            *slash = 0;
            if (dir[0] && make_dirs(dir) < 0) {
                cz_fail(e, E_IO, "%s: %s", dir, strerror(errno));
                free(dir);
                sb_free(&icc);
                processed_free(img);
                return -1;
            }
        }
    }
    free(dir);
    if (tiff_write(job->path, img, job->sixteen, (const uint8_t *)icc.p, icc.len, e) < 0) {
        sb_free(&icc);
        processed_free(img);
        return -1;
    }
    sb_free(&icc);
    memset(out, 0, sizeof *out);
    out->file_name = xstrdup(base_name(job->path));
    out->frame = xstrdup(job->name);
    out->path = xstrdup(job->path);
    out->bytes = stat(job->path, &st) == 0 ? (uint64_t)st.st_size : 0;
    out->width = img->width;
    out->height = img->height;
    out->channels = img->channels;
    out->bits = job->sixteen ? 16 : 8;
    out->created = unix_time();
    processed_free(img);
    return 0;
}

static void *scanner_thread(void *arg)
{
    cz_scanner *scanner = NULL;
    (void)arg;

    for (;;) {
        request *r;
        cz_err e;

        lock();
        while (S.queue_head == NULL)
            pthread_cond_wait(&S.wake, &S.lock);
        r = S.queue_head;
        S.queue_head = r->next;
        if (S.queue_head == NULL)
            S.queue_tail = NULL;
        unlock();
        S.cancel = 0;
        memset(&e, 0, sizeof e);

        switch (r->kind) {
        case REQ_CONNECT: {
            char label[400], d[400];
            cz_transport *t;
            scanner_close(scanner);
            scanner = NULL;
            lock();
            free(S.connected);
            S.connected = NULL;
            conn_label(r, label, sizeof label);
            slog_locked(0, "Connecting to %s", label);
            unlock();
            t = open_connection(r, &e);
            if (t != NULL)
                scanner = scanner_open(t, &e);
            if (scanner != NULL) {
                scanner_describe(scanner, d, sizeof d);
                lock();
                slog_locked(0, "Connected: %s", d);
                S.connected = xstrdup(d);
                unlock();
            } else {
                slog(1, "Connection failed: %s", e.msg);
            }
            break;
        }
        case REQ_DISCONNECT:
            scanner_close(scanner);
            scanner = NULL;
            lock();
            free(S.connected);
            S.connected = NULL;
            slog_locked(0, "Disconnected");
            unlock();
            break;
        case REQ_PROBE:
            if (scanner == NULL) {
                slog(1, "Not connected");
            } else {
                char *rep = scanner_probe(scanner, &e);
                if (rep != NULL) {
                    slog(0, "%s", rep);
                    free(rep);
                } else {
                    slog(1, "Probe failed: %s", e.msg);
                }
            }
            break;
        case REQ_PRESCAN: {
            cz_raw *raw = NULL;
            if (scanner != NULL)
                raw = scanner_preview(scanner, &r->window, on_progress, NULL, &S.cancel, &e);
            else
                cz_fail(&e, E_INVALID, "not connected");
            if (raw != NULL) {
                prescan *p = xcalloc(1, sizeof *p);
                prescan *old;
                p->refs = 1;
                p->raw = raw;
                sb_init(&p->png);
                display_png(raw, &p->png);
                p->bed_w = r->window.area.width_mm;
                p->bed_h = r->window.area.height_mm;
                lock();
                old = S.pre;
                S.pre = p;
                S.prescan_seq++;
                slog_locked(0, "Prescan finished");
                S.busy = 0;
                unlock();
                prescan_unref(old);
            } else {
                lock();
                slog_locked(1, "Prescan failed: %s", e.msg);
                S.busy = 0;
                unlock();
            }
            break;
        }
        case REQ_SCAN: {
            int n = r->njobs, ok = 0, i;
            if (n > 0) {
                char dpi[64];
                snprintf(dpi, sizeof dpi, "%g", r->jobs[0].window.x_dpi);
                if (n == 1)
                    slog(0, "Scanning %s at %s dpi", r->jobs[0].name, dpi);
                else
                    slog(0, "Scanning %d frames at %s dpi", n, dpi);
            }
            for (i = 0; i < n; i++) {
                frame_job *job = &r->jobs[i];
                scan_file f;
                if (scanner == NULL) {
                    slog(1, "Not connected");
                    break;
                }
                lock();
                snprintf(S.task, sizeof S.task, "Scanning %s (%d of %d)", job->name, i + 1, n);
                unlock();
                if (scan_one(scanner, job, &f, &e) == 0) {
                    ok++;
                    lock();
                    f.id = ++S.file_seq;
                    slog_locked(0, "Saved %s as %s", job->name, f.file_name);
                    push_file_locked(&f);
                    unlock();
                } else {
                    slog(1, "%s: %s", job->name, e.msg);
                    if (S.cancel)
                        break;
                }
            }
            lock();
            slog_locked(0, "Scanned %d of %d frames", ok, n);
            S.busy = 0;
            unlock();
            break;
        }
        }
        request_free(r);
    }
    return NULL;
}

/* ------------------------------------------------------------------ HTTP */

typedef struct {
    char method[16];
    char path[2048];
    char query[2048];
    char *head;
    int minor; /* HTTP/1.minor */
    char *body;
    size_t body_len;
} http_req;

typedef struct {
    sock_t s;
    int keep_alive;
} client;

static const char *status_text(int code)
{
    switch (code) {
    case 100: return "Continue";
    case 200: return "OK";
    case 400: return "Bad Request";
    case 401: return "Unauthorized";
    case 404: return "Not Found";
    case 405: return "Method Not Allowed";
    case 409: return "Conflict";
    case 413: return "Payload Too Large";
    case 422: return "Unprocessable Entity";
    case 500: return "Internal Server Error";
    default: return "Error";
    }
}

/* the value of a header, a new string, or NULL. Several headers of one name are joined
 * with "; " which suits Cookie. */
static char *header(const http_req *r, const char *name)
{
    size_t nl = strlen(name);
    const char *p = r->head;
    sbuf out;
    int found = 0;

    sb_init(&out);
    p = strstr(p, "\r\n");
    while (p != NULL && p[2] != '\r' && p[2] != 0) {
        const char *line = p + 2, *end = strstr(line, "\r\n");
        if (end == NULL)
            break;
        if ((size_t)(end - line) > nl && line[nl] == ':' && strncasecmp(line, name, nl) == 0) {
            const char *v = line + nl + 1;
            const char *ve = end;
            while (v < ve && (*v == ' ' || *v == '\t'))
                v++;
            while (ve > v && (ve[-1] == ' ' || ve[-1] == '\t'))
                ve--;
            if (found)
                sb_puts(&out, "; ");
            sb_put(&out, v, (size_t)(ve - v));
            found = 1;
        }
        p = end;
    }
    if (!found) {
        sb_free(&out);
        return NULL;
    }
    return (char *)(sb_str(&out), out.p);
}

static int send_head(client *c, int code, const char *type, uint64_t length, const char *extra)
{
    sbuf h;
    int r;
    sb_init(&h);
    sb_printf(&h, "HTTP/1.1 %d %s\r\n", code, status_text(code));
    if (type != NULL)
        sb_printf(&h, "Content-Type: %s\r\n", type);
    sb_printf(&h, "Content-Length: %llu\r\n", (unsigned long long)length);
    if (extra != NULL)
        sb_puts(&h, extra);
    sb_puts(&h, c->keep_alive ? "Connection: keep-alive\r\n\r\n" : "Connection: close\r\n\r\n");
    r = sock_write_full(c->s, h.p, h.len);
    sb_free(&h);
    return r;
}

static int send_body(client *c, int code, const char *type, const void *body, size_t len, const char *extra)
{
    if (send_head(c, code, type, len, extra) < 0)
        return -1;
    return len ? sock_write_full(c->s, body, len) : 0;
}

static int send_json(client *c, int code, sbuf *b)
{
    return send_body(c, code, "application/json", b->p ? b->p : "", b->len, NULL);
}

static int send_error(client *c, int code, const char *msg)
{
    sbuf b;
    int r;
    sb_init(&b);
    sb_puts(&b, "{\"error\":");
    js_str(&b, msg);
    sb_byte(&b, '}');
    r = send_json(c, code, &b);
    sb_free(&b);
    return r;
}

static int send_ok(client *c)
{
    static const char ok[] = "{\"ok\":true}";
    return send_body(c, 200, "application/json", ok, sizeof ok - 1, NULL);
}

/* reads one request. 0 ok, -1 connection closed or broken (nothing to answer), or an
 * HTTP status to answer with before closing */
static int read_request(client *c, http_req *r, sbuf *in)
{
    char *end, *line_end, *cl, *expect;
    size_t head_len;
    unsigned long long content_length = 0;

    memset(r, 0, sizeof *r);
    for (;;) {
        char buf[16384];
        int n;
        if (in->len > 0 && (end = strstr(in->p, "\r\n\r\n")) != NULL)
            break;
        if (in->len > MAX_HEAD)
            return 413;
        n = (int)recv(c->s, buf, sizeof buf, 0);
        if (n <= 0)
            return -1;
        sb_put(in, buf, (size_t)n);
    }
    head_len = (size_t)(end - in->p) + 4;
    r->head = xstrndup(in->p, head_len);
    line_end = strstr(r->head, "\r\n");
    {
        char target[4096], version[16];
        char *q;
        *line_end = 0;
        if (sscanf(r->head, "%15s %4095s %15s", r->method, target, version) != 3 ||
            strncmp(version, "HTTP/1.", 7) != 0) {
            *line_end = '\r';
            return 400;
        }
        *line_end = '\r';
        r->minor = version[7] - '0';
        q = strchr(target, '?');
        if (q != NULL) {
            snprintf(r->query, sizeof r->query, "%s", q + 1);
            *q = 0;
        }
        snprintf(r->path, sizeof r->path, "%s", target);
    }
    {
        char *conn = header(r, "Connection");
        c->keep_alive = r->minor >= 1;
        if (conn != NULL) {
            if (strcasecmp(conn, "close") == 0)
                c->keep_alive = 0;
            else if (strcasecmp(conn, "keep-alive") == 0)
                c->keep_alive = 1;
            free(conn);
        }
    }
    cl = header(r, "Content-Length");
    if (cl != NULL) {
        content_length = strtoull(cl, NULL, 10);
        free(cl);
    }
    {
        char *te = header(r, "Transfer-Encoding");
        if (te != NULL) {
            free(te);
            return 400; /* browsers send a length for fetch bodies */
        }
    }
    if (content_length > MAX_BODY)
        return 413;
    expect = header(r, "Expect");
    if (expect != NULL) {
        if (strcasecmp(expect, "100-continue") == 0 && in->len - head_len < content_length)
            sock_write_full(c->s, "HTTP/1.1 100 Continue\r\n\r\n", 25);
        free(expect);
    }
    while (in->len - head_len < content_length) {
        char buf[65536];
        int n = (int)recv(c->s, buf, sizeof buf, 0);
        if (n <= 0)
            return -1;
        sb_put(in, buf, (size_t)n);
    }
    r->body_len = (size_t)content_length;
    r->body = xstrndup(in->p + head_len, r->body_len);
    /* keep whatever follows, a pipelined next request */
    memmove(in->p, in->p + head_len + r->body_len, in->len - head_len - r->body_len);
    in->len -= head_len + r->body_len;
    in->p[in->len] = 0;
    return 0;
}

static void request_done(http_req *r)
{
    free(r->head);
    free(r->body);
}

static int hexval(char c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'a' && c <= 'f')
        return c - 'a' + 10;
    if (c >= 'A' && c <= 'F')
        return c - 'A' + 10;
    return -1;
}

/* a query parameter, URL decoded, into out. 0 when present */
static int query_param(const http_req *r, const char *name, char *out, size_t size)
{
    const char *p = r->query;
    size_t nl = strlen(name);
    while (*p) {
        const char *amp = strchr(p, '&');
        const char *end = amp ? amp : p + strlen(p);
        if ((size_t)(end - p) >= nl && strncmp(p, name, nl) == 0 && (p[nl] == '=' || p + nl == end)) {
            const char *v = p[nl] == '=' ? p + nl + 1 : end;
            size_t o = 0;
            while (v < end && o + 1 < size) {
                if (*v == '%' && end - v >= 3 && hexval(v[1]) >= 0 && hexval(v[2]) >= 0) {
                    out[o++] = (char)(hexval(v[1]) * 16 + hexval(v[2]));
                    v += 3;
                } else {
                    out[o++] = *v == '+' ? ' ' : *v;
                    v++;
                }
            }
            out[o] = 0;
            return 0;
        }
        p = amp ? amp + 1 : end;
    }
    return -1;
}

/* ------------------------------------------------------------------ access token */

#define COOKIE "cezanne_token"

static int same(const char *a, size_t al, const char *b, size_t bl)
{
    unsigned char acc = 0;
    size_t i;
    if (al != bl)
        return 0;
    for (i = 0; i < al; i++)
        acc |= (unsigned char)(a[i] ^ b[i]);
    return acc == 0;
}

static int authorised(const http_req *r)
{
    char *h, *cookie;
    int ok = 0;
    size_t tl;

    if (S.token == NULL)
        return 1;
    tl = strlen(S.token);
    h = header(r, "X-Cezanne-Token");
    if (h != NULL) {
        ok = same(h, strlen(h), S.token, tl);
        free(h);
        if (ok)
            return 1;
    }
    cookie = header(r, "Cookie");
    if (cookie != NULL) {
        char *p = cookie;
        while (p != NULL && !ok) {
            char *semi = strchr(p, ';'), *kv, *eq;
            if (semi != NULL)
                *semi = 0;
            kv = p;
            trim_in_place(kv);
            eq = strchr(kv, '=');
            if (eq != NULL && (size_t)(eq - kv) == strlen(COOKIE) && strncmp(kv, COOKIE, strlen(COOKIE)) == 0)
                ok = same(eq + 1, strlen(eq + 1), S.token, tl);
            p = semi ? semi + 1 : NULL;
        }
        free(cookie);
    }
    return ok;
}

/* ------------------------------------------------------------------ JSON bodies */

static jv *parse_body(client *c, const http_req *r)
{
    char err[200];
    jv *v = json_parse(r->body ? r->body : "", r->body_len, err, sizeof err);
    if (v == NULL) {
        char msg[300];
        snprintf(msg, sizeof msg, "Failed to parse the request body as JSON: %s", err);
        send_error(c, 400, msg);
    }
    return v;
}

static int get_num(const jv *o, const char *key, double *out)
{
    jv *v = jget(o, key);
    if (v == NULL || v->type != J_NUM)
        return -1;
    *out = v->n;
    return 0;
}

static const char *get_str(const jv *o, const char *key)
{
    jv *v = jget(o, key);
    return v != NULL && v->type == J_STR ? v->s : NULL;
}

static int get_area(const jv *o, cz_area *a)
{
    return get_num(o, "x_mm", &a->x_mm) < 0 || get_num(o, "y_mm", &a->y_mm) < 0 ||
                   get_num(o, "width_mm", &a->width_mm) < 0 || get_num(o, "height_mm", &a->height_mm) < 0
               ? -1
               : 0;
}

typedef struct {
    int area_unit;
    uint32_t res_scale;
} advanced;

/* the Advanced section, SCREEN's driver values by default */
static int get_advanced(const jv *o, advanced *a)
{
    jv *v = jget(o, "advanced");
    a->area_unit = UNIT_TENTH_MM;
    a->res_scale = 10;
    if (v == NULL || v->type == J_NULL)
        return 0;
    if (v->type != J_OBJ)
        return -1;
    if (jget(v, "area_unit") != NULL) {
        const char *u = get_str(v, "area_unit");
        if (u == NULL || (a->area_unit = unit_from_id(u)) < 0)
            return -1;
    }
    if (jget(v, "res_scale") != NULL) {
        double d;
        if (get_num(v, "res_scale", &d) < 0 || d != floor(d) || d < 0 || d > 4294967295.0)
            return -1;
        a->res_scale = (uint32_t)d;
    }
    return 0;
}

static void make_window(cz_window *w, cz_area area, double dpi, int mode, int film, const advanced *adv)
{
    window_init(w, area, dpi, mode);
    w->original = film_original(film);
    w->area_unit = adv->area_unit;
    w->resolution_scale = adv->res_scale < 1 ? 1 : adv->res_scale > 100 ? 100 : adv->res_scale;
}

/* ------------------------------------------------------------------ presets */

typedef struct {
    char *name;
    int mode;
    int reflective;
    double dpi;
    cz_adjust adj;
    int sixteen;
    int auto_levels;
} preset;

static void preset_new(preset *p, const char *name, int film)
{
    memset(p, 0, sizeof *p);
    p->name = xstrdup(name);
    p->mode = MODE_RGB48;
    p->reflective = film == FILM_REFLECTIVE;
    p->dpi = 2000.0;
    adjust_for_film(&p->adj, film);
    p->sixteen = 1;
    p->auto_levels = 1;
}

/* the built in presets from the workflows Cezanne owners shared (preset.rs) */
static int builtin_presets(preset *out)
{
    preset_new(&out[0], "Colour slide", FILM_POSITIVE);
    preset_new(&out[1], "Colour negative", FILM_COLOR_NEGATIVE);
    adjust_s_curve(&out[1].adj, 1.0f);
    preset_new(&out[2], "B&W negative (flipped HD/SD)", FILM_BW_NEGATIVE);
    {
        int c;
        for (c = 0; c < 3; c++) {
            out[2].adj.hd[c] = 2.2f;
            out[2].adj.sd[c] = 0.1f;
        }
    }
    adjust_s_curve(&out[2].adj, 0.8f);
    out[2].adj.gray = 1;
    preset_new(&out[3], "B&W negative (best channel)", FILM_BW_NEGATIVE);
    out[3].auto_levels = 1;
    out[3].adj.gray = 1;
    adjust_linear_curve(&out[3].adj);
    preset_new(&out[4], "Print (reflective)", FILM_REFLECTIVE);
    out[4].dpi = 600.0;
    return 5;
}

static void preset_json(sbuf *b, const preset *p, int builtin)
{
    sb_printf(b, "{\"builtin\":%s,\"name\":", builtin ? "true" : "false");
    js_str(b, p->name);
    sb_puts(b, ",\"mode\":");
    js_str(b, mode_id(p->mode));
    sb_puts(b, ",\"light\":");
    js_str(b, p->reflective ? "Reflective" : "Transmissive");
    sb_puts(b, ",\"dpi\":");
    js_f64(b, p->dpi);
    sb_puts(b, ",\"adjustments\":");
    adjust_to_json(b, &p->adj);
    sb_printf(b, ",\"output_depth\":\"%s\",\"auto_levels\":%s}", p->sixteen ? "Sixteen" : "Eight",
              p->auto_levels ? "true" : "false");
}

static int preset_from_json(const jv *o, preset *p, char *err, size_t errlen)
{
    const char *name = get_str(o, "name"), *mode = get_str(o, "mode"), *light = get_str(o, "light");
    const char *depth = get_str(o, "output_depth");
    jv *al = jget(o, "auto_levels");

    memset(p, 0, sizeof *p);
    if (name == NULL || mode == NULL || (p->mode = mode_from_id(mode)) < 0 || light == NULL ||
        (strcmp(light, "Reflective") != 0 && strcmp(light, "Transmissive") != 0) || get_num(o, "dpi", &p->dpi) < 0 ||
        depth == NULL || (strcmp(depth, "Sixteen") != 0 && strcmp(depth, "Eight") != 0) || al == NULL ||
        al->type != J_BOOL) {
        snprintf(err, errlen, "a preset needs name, mode, light, dpi, adjustments, output_depth and auto_levels");
        return -1;
    }
    if (adjust_from_json(jget(o, "adjustments"), &p->adj, err, errlen) < 0)
        return -1;
    p->name = xstrdup(name);
    p->reflective = strcmp(light, "Reflective") == 0;
    p->sixteen = strcmp(depth, "Sixteen") == 0;
    p->auto_levels = al->b;
    return 0;
}

static char *preset_dir(void)
{
    const char *home = getenv("HOME");
    char base[1024];
#ifdef _WIN32
    const char *appdata = getenv("APPDATA");
    if (home == NULL)
        home = getenv("USERPROFILE");
    snprintf(base, sizeof base, "%s\\Cezanne Scan\\presets", appdata ? appdata : home ? home : ".");
#elif defined(__APPLE__)
    snprintf(base, sizeof base, "%s/Library/Application Support/Cezanne Scan/presets", home ? home : "");
#else
    const char *xdg = getenv("XDG_CONFIG_HOME");
    if (xdg != NULL)
        snprintf(base, sizeof base, "%s/cezanne-scan/presets", xdg);
    else
        snprintf(base, sizeof base, "%s/.config/cezanne-scan/presets", home ? home : "");
#endif
    return xstrdup(base);
}

static int preset_name_cmp(const void *a, const void *b)
{
    return strcmp(((const preset *)a)->name, ((const preset *)b)->name);
}

static void read_file(const char *path, sbuf *out)
{
    FILE *f = fopen(path, "rb");
    char buf[8192];
    size_t n;
    if (f == NULL)
        return;
    while ((n = fread(buf, 1, sizeof buf, f)) > 0 && out->len < (1u << 20))
        sb_put(out, buf, n);
    fclose(f);
}

static void handle_presets(client *c)
{
    preset builtin[8], *user = NULL;
    int nb = builtin_presets(builtin), i;
    size_t nu = 0, cap = 0, k;
    char *dir = preset_dir();
    DIR *d = opendir(dir);
    sbuf b;

    if (d != NULL) {
        struct dirent *de;
        while ((de = readdir(d)) != NULL) {
            size_t l = strlen(de->d_name);
            char *path;
            sbuf text;
            jv *v;
            preset p;
            char err[200];
            if (l < 5 || strcmp(de->d_name + l - 5, ".json") != 0)
                continue;
            path = join_path(dir, de->d_name);
            sb_init(&text);
            read_file(path, &text);
            free(path);
            v = json_parse(text.p ? text.p : "", text.len, err, sizeof err);
            sb_free(&text);
            if (v != NULL && preset_from_json(v, &p, err, sizeof err) == 0) {
                if (nu == cap) {
                    cap = cap ? cap * 2 : 8;
                    user = xrealloc(user, cap * sizeof *user);
                }
                user[nu++] = p;
            }
            json_free(v);
        }
        closedir(d);
    }
    free(dir);
    qsort(user, nu, sizeof *user, preset_name_cmp);
    sb_init(&b);
    sb_puts(&b, "{\"presets\":[");
    for (i = 0; i < nb; i++) {
        if (i)
            sb_byte(&b, ',');
        preset_json(&b, &builtin[i], 1);
    }
    for (k = 0; k < nu; k++) {
        sb_byte(&b, ',');
        preset_json(&b, &user[k], 0);
    }
    sb_puts(&b, "]}");
    send_json(c, 200, &b);
    sb_free(&b);
    for (i = 0; i < nb; i++)
        free(builtin[i].name);
    for (k = 0; k < nu; k++)
        free(user[k].name);
    free(user);
}

static void handle_save_preset(client *c, const http_req *r)
{
    jv *v = parse_body(c, r);
    preset p;
    char err[300], *dir, *safe, *path, *s;
    size_t i, n;
    sbuf b;
    FILE *f;

    if (v == NULL)
        return;
    if (preset_from_json(v, &p, err, sizeof err) < 0) {
        json_free(v);
        send_error(c, 422, err);
        return;
    }
    json_free(v);
    s = xstrdup(p.name);
    trim_in_place(s);
    if (s[0] == 0) {
        free(s);
        free(p.name);
        send_error(c, 400, "Give the preset a name");
        return;
    }
    free(s);
    /* letters, digits and spaces stay, everything else becomes _ (bytes of UTF-8 letters stay) */
    n = strlen(p.name);
    safe = xmalloc(n + 6);
    for (i = 0; i < n; i++) {
        unsigned char ch = (unsigned char)p.name[i];
        safe[i] = (isalnum(ch) || ch == ' ' || ch >= 0x80) ? (char)ch : '_';
    }
    safe[n] = 0;
    trim_in_place(safe);
    strcat(safe, ".json");
    dir = preset_dir();
    if (make_dirs(dir) < 0) {
        snprintf(err, sizeof err, "I/O error: %s: %s", dir, strerror(errno));
        free(dir);
        free(safe);
        free(p.name);
        send_error(c, 500, err);
        return;
    }
    path = join_path(dir, safe);
    free(dir);
    free(safe);
    sb_init(&b);
    preset_json(&b, &p, 0);
    /* saved without the builtin flag, as the Rust server saves it */
    {
        const char *body = b.p + strlen("{\"builtin\":false,");
        f = fopen(path, "wb");
        if (f == NULL || fputc('{', f) == EOF || fwrite(body, 1, strlen(body), f) != strlen(body) || fclose(f) != 0) {
            snprintf(err, sizeof err, "I/O error: %s: %s", path, strerror(errno));
            sb_free(&b);
            free(path);
            free(p.name);
            send_error(c, 500, err);
            return;
        }
    }
    sb_free(&b);
    slog(0, "Saved preset %s to %s", p.name, path);
    free(path);
    free(p.name);
    send_ok(c);
}

/* ------------------------------------------------------------------ handlers */

static const char *os_name(void)
{
#ifdef __APPLE__
    return "macos";
#elif defined(_WIN32)
    return "windows";
#else
    return "linux";
#endif
}

static void handle_info(client *c, const http_req *r)
{
    sbuf b;
    sb_init(&b);
    sb_printf(&b, "{\"name\":\"Cezanne Scan server\",\"version\":\"%s\",\"needs_token\":%s,\"authorised\":%s}",
              CZ_VERSION, S.token ? "true" : "false", authorised(r) ? "true" : "false");
    send_json(c, 200, &b);
    sb_free(&b);
}

static void handle_login(client *c, const http_req *r)
{
    jv *v = parse_body(c, r);
    const char *tok;
    char *t, extra[600];
    size_t i;
    int printable = 1;

    if (v == NULL)
        return;
    tok = get_str(v, "token");
    if (tok == NULL) {
        json_free(v);
        send_error(c, 422, "missing field `token`");
        return;
    }
    t = xstrdup(tok);
    json_free(v);
    trim_in_place(t);
    if (S.token != NULL && !same(t, strlen(t), S.token, strlen(S.token))) {
        free(t);
        send_error(c, 401, "Wrong access token");
        return;
    }
    for (i = 0; t[i]; i++)
        if ((unsigned char)t[i] < 0x20 || t[i] == 0x7F)
            printable = 0;
    if (printable && strlen(t) < 400) {
        static const char ok[] = "{\"ok\":true}";
        snprintf(extra, sizeof extra,
                 "Set-Cookie: " COOKIE "=%s; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000\r\n", t);
        send_body(c, 200, "application/json", ok, sizeof ok - 1, extra);
    } else {
        send_ok(c);
    }
    free(t);
}

static void file_json(sbuf *b, const scan_file *f)
{
    sb_printf(b, "{\"id\":%llu,\"file_name\":", (unsigned long long)f->id);
    js_str(b, f->file_name);
    sb_puts(b, ",\"frame\":");
    js_str(b, f->frame);
    sb_printf(b, ",\"bytes\":%llu,\"width\":%u,\"height\":%u,\"channels\":%d,\"bits\":%d,\"created\":%llu}",
              (unsigned long long)f->bytes, f->width, f->height, f->channels, f->bits,
              (unsigned long long)f->created);
}

static void handle_state(client *c, const http_req *r)
{
    char q[64];
    unsigned long long after = 0;
    double tw, th, iw, ih;
    sbuf b;
    size_t i;
    int k, first = 1;

    if (query_param(r, "log_after", q, sizeof q) == 0)
        after = strtoull(q, NULL, 10);
    limits_bed_mm(LIMITS, UNIT_TENTH_MM, &tw, &th);
    limits_bed_mm(LIMITS, UNIT_INCH1200, &iw, &ih);
    sb_init(&b);
    lock();
    sb_printf(&b, "{\"version\":\"%s\",\"host\":", CZ_VERSION);
    js_str(&b, S.host);
    sb_puts(&b, ",\"os\":");
    js_str(&b, os_name());
    sb_puts(&b, ",\"connected\":");
    if (S.connected)
        js_str(&b, S.connected);
    else
        sb_puts(&b, "null");
    sb_printf(&b, ",\"busy\":%s,\"task\":", S.busy ? "true" : "false");
    js_str(&b, S.task);
    sb_puts(&b, ",\"progress\":");
    js_f32(&b, S.progress);
    sb_printf(&b, ",\"prescan_seq\":%llu,\"prescan_bed_mm\":", (unsigned long long)S.prescan_seq);
    if (S.pre != NULL) {
        sb_byte(&b, '[');
        js_f64(&b, S.pre->bed_w);
        sb_byte(&b, ',');
        js_f64(&b, S.pre->bed_h);
        sb_byte(&b, ']');
    } else {
        sb_puts(&b, "null");
    }
    sb_puts(&b, ",\"bed_mm\":{\"TenthMm\":[");
    js_f64(&b, tw);
    sb_byte(&b, ',');
    js_f64(&b, th);
    sb_puts(&b, "],\"Inch1200\":[");
    js_f64(&b, iw);
    sb_byte(&b, ',');
    js_f64(&b, ih);
    sb_printf(&b, "]},\"max_pixels_per_line\":%u,\"output_dir\":", LIMITS->max_pixels_per_line);
    js_str(&b, S.output_dir);
    sb_puts(&b, ",\"start_kind\":");
    if (S.start_kind)
        js_str(&b, S.start_kind);
    else
        sb_puts(&b, "null");
    sb_puts(&b, ",\"files\":[");
    for (i = 0; i < S.nfiles; i++) {
        if (i)
            sb_byte(&b, ',');
        file_json(&b, &S.files[i]);
    }
    sb_puts(&b, "],\"log\":[");
    for (k = 0; k < S.log_count; k++) {
        const log_entry *l = &S.log[(S.log_start + k) % LOG_LINES];
        if (l->seq <= after)
            continue;
        if (!first)
            sb_byte(&b, ',');
        first = 0;
        sb_printf(&b, "{\"seq\":%llu,\"text\":", (unsigned long long)l->seq);
        js_str(&b, l->text);
        sb_printf(&b, ",\"error\":%s}", l->error ? "true" : "false");
    }
    sb_printf(&b, "],\"log_seq\":%llu}", (unsigned long long)S.log_seq);
    unlock();
    send_json(c, 200, &b);
    sb_free(&b);
}

/* film types with their starting adjustments and the colour modes, so the page uses the
 * same defaults as the desktop app */
static void handle_meta(client *c)
{
    sbuf b;
    int f, m;
    sb_init(&b);
    sb_puts(&b, "{\"films\":[");
    for (f = 0; f < 4; f++) {
        cz_adjust a;
        int o = film_original(f);
        if (f)
            sb_byte(&b, ',');
        sb_puts(&b, "{\"id\":");
        js_str(&b, film_id(f));
        sb_puts(&b, ",\"label\":");
        js_str(&b, film_label(f));
        sb_printf(&b, ",\"negative\":%s,\"original\":", film_is_negative(f) ? "true" : "false");
        js_str(&b, original_label(o));
        sb_printf(&b, ",\"reflective\":%s,\"adjustments\":", original_is_reflective(o) ? "true" : "false");
        adjust_for_film(&a, f);
        adjust_to_json(&b, &a);
        sb_byte(&b, '}');
    }
    sb_puts(&b, "],\"modes\":[");
    for (m = 0; m < 4; m++) {
        if (m)
            sb_byte(&b, ',');
        sb_puts(&b, "{\"id\":");
        js_str(&b, mode_id(m));
        sb_puts(&b, ",\"label\":");
        js_str(&b, mode_label(m));
        sb_printf(&b, ",\"channels\":%d,\"bytes\":%d}", mode_channels(m), mode_bytes_per_sample(m));
    }
    sb_puts(&b, "]}");
    send_json(c, 200, &b);
    sb_free(&b);
}

static int is_busy(void)
{
    int b;
    lock();
    b = S.busy;
    unlock();
    return b;
}

static void submit(client *c, request *rq)
{
    char why[400];
    if (send_request(rq, why, sizeof why) != NULL)
        send_error(c, 409, why);
    else
        send_ok(c);
}

static void handle_connect(client *c, const http_req *r)
{
    jv *v;
    const char *kind;
    request *rq;

    if (is_busy()) {
        send_error(c, 409, "Wait for the current scan to finish");
        return;
    }
    v = parse_body(c, r);
    if (v == NULL)
        return;
    kind = get_str(v, "kind");
    rq = xcalloc(1, sizeof *rq);
    rq->kind = REQ_CONNECT;
    if (kind != NULL && strcmp(kind, "Simulator") == 0) {
        rq->conn = CONN_SIM;
    } else if (kind != NULL && strcmp(kind, "Local") == 0) {
        const char *dev = get_str(v, "device");
        char *t = xstrdup(dev ? dev : "");
        trim_in_place(t);
        if (t[0] == 0) {
            rq->conn = CONN_AUTO;
            free(t);
        } else {
            rq->conn = CONN_LOCAL;
            rq->address = t;
        }
    } else if (kind != NULL && strcmp(kind, "Bridge") == 0 && get_str(v, "host") != NULL) {
        const char *tok = get_str(v, "token");
        rq->conn = CONN_BRIDGE;
        rq->address = xstrdup(get_str(v, "host"));
        rq->token = xstrdup(tok ? tok : "");
    } else {
        json_free(v);
        free(rq);
        send_error(c, 422, "kind must be Simulator, Local or Bridge (with host)");
        return;
    }
    json_free(v);
    submit(c, rq);
}

static void handle_simple(client *c, int kind)
{
    request *rq;
    if (is_busy()) {
        send_error(c, 409, "Wait for the current scan to finish");
        return;
    }
    rq = xcalloc(1, sizeof *rq);
    rq->kind = kind;
    submit(c, rq);
}

static void handle_devices(client *c)
{
    cz_device d[16];
    cz_err e;
    int n = cz_discover(d, 16, &e), i;
    sbuf b;
    sb_init(&b);
    sb_puts(&b, "{\"devices\":[");
    for (i = 0; i < n; i++) {
        if (i)
            sb_byte(&b, ',');
        sb_puts(&b, "{\"address\":");
        js_str(&b, d[i].address);
        sb_puts(&b, ",\"vendor\":");
        js_str(&b, d[i].vendor);
        sb_puts(&b, ",\"product\":");
        js_str(&b, d[i].product);
        sb_puts(&b, ",\"revision\":");
        js_str(&b, d[i].revision);
        sb_byte(&b, '}');
    }
    sb_byte(&b, ']');
    if (n < 0) {
        sb_puts(&b, ",\"error\":");
        js_str(&b, e.msg);
    }
    sb_byte(&b, '}');
    send_json(c, 200, &b);
    sb_free(&b);
}

static void handle_cancel(client *c)
{
    S.cancel = 1;
    slog(0, "Cancel requested");
    send_ok(c);
}

static void handle_prescan(client *c, const http_req *r)
{
    jv *v = parse_body(c, r);
    double dpi, bw, bh;
    const char *film;
    int f;
    advanced adv;
    request *rq;
    cz_area area;

    if (v == NULL)
        return;
    film = get_str(v, "film");
    if (get_num(v, "dpi", &dpi) < 0 || film == NULL || (f = film_from_id(film)) < 0 || get_advanced(v, &adv) < 0) {
        json_free(v);
        send_error(c, 422, "a prescan needs dpi, film and optionally advanced");
        return;
    }
    json_free(v);
    if (!(dpi >= 20.0 && dpi <= 300.0)) {
        send_error(c, 400, "Prescan resolution must be 20 to 300 dpi");
        return;
    }
    limits_bed_mm(LIMITS, adv.area_unit, &bw, &bh);
    area.x_mm = 0;
    area.y_mm = 0;
    area.width_mm = bw;
    area.height_mm = bh;
    rq = xcalloc(1, sizeof *rq);
    rq->kind = REQ_PRESCAN;
    make_window(&rq->window, area, dpi, MODE_RGB48, f, &adv);
    submit(c, rq);
}

static void handle_prescan_png(client *c)
{
    prescan *p = prescan_ref();
    if (p == NULL) {
        send_error(c, 404, "No prescan yet");
        return;
    }
    send_body(c, 200, "image/png", p->png.p, p->png.len, "Cache-Control: no-store\r\n");
    prescan_unref(p);
}

static uint32_t f2u(double v)
{
    if (!(v > 0))
        return 0;
    if (v >= 4294967295.0)
        return 0xFFFFFFFFu;
    return (uint32_t)v;
}

/* raw prescan pixels inside an area given in millimetres (Prescan::crop_mm) */
static cz_raw *crop_mm(const prescan *p, const cz_area *a)
{
    double sx = (double)p->raw->width / p->bed_w, sy = (double)p->raw->height / p->bed_h;
    uint32_t px = f2u(fmax(a->x_mm * sx, 0.0)), py = f2u(fmax(a->y_mm * sy, 0.0)), pw, ph;
    if (px > p->raw->width - 1)
        px = p->raw->width - 1;
    if (py > p->raw->height - 1)
        py = p->raw->height - 1;
    pw = f2u(fmax(a->width_mm * sx, 1.0));
    ph = f2u(fmax(a->height_mm * sy, 1.0));
    if (pw > p->raw->width - px)
        pw = p->raw->width - px;
    if (ph > p->raw->height - py)
        ph = p->raw->height - py;
    return raw_crop(p->raw, px, py, pw, ph);
}

static void handle_density(client *c, const http_req *r)
{
    char xs[64], ys[64], *ex, *ey;
    double x, y;
    prescan *p;
    sbuf b;

    if (query_param(r, "x", xs, sizeof xs) < 0 || query_param(r, "y", ys, sizeof ys) < 0) {
        send_error(c, 400, "Failed to deserialize query string: missing x or y");
        return;
    }
    x = strtod(xs, &ex);
    y = strtod(ys, &ey);
    if (*ex != 0 || *ey != 0 || xs[0] == 0 || ys[0] == 0) {
        send_error(c, 400, "Failed to deserialize query string: x and y must be numbers");
        return;
    }
    p = prescan_ref();
    sb_init(&b);
    sb_puts(&b, "{\"density\":");
    if (p == NULL) {
        sb_puts(&b, "null");
    } else {
        double fx = floor(x / p->bed_w * (double)p->raw->width), fy = floor(y / p->bed_h * (double)p->raw->height);
        if (fx < 0.0 || fy < 0.0 || fx >= (double)p->raw->width || fy >= (double)p->raw->height || fx != fx ||
            fy != fy) {
            sb_puts(&b, "null");
        } else {
            uint32_t ix = (uint32_t)fx, iy = (uint32_t)fy, w, h;
            double d[3];
            int k;
            ix = ix ? ix - 1 : 0;
            iy = iy ? iy - 1 : 0;
            w = p->raw->width - ix < 3 ? p->raw->width - ix : 3;
            h = p->raw->height - iy < 3 ? p->raw->height - iy : 3;
            raw_density_at(p->raw, ix, iy, w, h, d);
            sb_byte(&b, '[');
            for (k = 0; k < p->raw->channels; k++) {
                if (k)
                    sb_byte(&b, ',');
                js_f64(&b, d[k]);
            }
            sb_byte(&b, ']');
        }
        prescan_unref(p);
    }
    sb_byte(&b, '}');
    send_json(c, 200, &b);
    sb_free(&b);
}

/* the processed preview of one frame. With auto_levels the HD and SD found come back in
 * the X-Adjustments header so the page can show them */
static void handle_preview(client *c, const http_req *r)
{
    jv *v = parse_body(c, r), *al;
    cz_area area;
    cz_adjust adj;
    char err[300];
    prescan *p;
    cz_raw *raw;
    cz_processed *img;
    uint8_t *rgba;
    sbuf png, hdr, extra;

    if (v == NULL)
        return;
    if (get_area(jget(v, "area"), &area) < 0) {
        json_free(v);
        send_error(c, 422, "area: expected x_mm, y_mm, width_mm and height_mm");
        return;
    }
    if (adjust_from_json(jget(v, "adjustments"), &adj, err, sizeof err) < 0) {
        json_free(v);
        send_error(c, 422, err);
        return;
    }
    al = jget(v, "auto_levels");
    p = prescan_ref();
    if (p == NULL) {
        json_free(v);
        send_error(c, 404, "No prescan yet");
        return;
    }
    if (area.width_mm <= 0.0 || area.height_mm <= 0.0) {
        json_free(v);
        prescan_unref(p);
        send_error(c, 400, "The frame is empty");
        return;
    }
    raw = crop_mm(p, &area);
    prescan_unref(p);
    if (al != NULL && al->type == J_BOOL && al->b)
        adjust_auto_levels(&adj, raw, 0.002);
    json_free(v);
    img = adjust_apply(&adj, raw);
    raw_free(raw);
    rgba = processed_to_rgba8(img);
    sb_init(&png);
    png_encode_rgba(img->width, img->height, rgba, &png);
    free(rgba);
    processed_free(img);
    sb_init(&hdr);
    adjust_to_json(&hdr, &adj);
    sb_init(&extra);
    sb_puts(&extra, "Cache-Control: no-store\r\nX-Adjustments: ");
    sb_put(&extra, hdr.p, hdr.len);
    sb_puts(&extra, "\r\n");
    send_body(c, 200, "image/png", png.p, png.len, extra.p);
    sb_free(&png);
    sb_free(&hdr);
    sb_free(&extra);
}

/* keep file name prefixes to plain characters so a browser cannot write outside the folder */
static void clean_prefix(const char *in, char *out, size_t size)
{
    char tmp[64];
    size_t n = 0, i;
    const char *s;
    int count = 0;
    /* walk characters (UTF-8 sequences count as one and are dropped) */
    for (s = in; *s && count < 40;) {
        unsigned char ch = (unsigned char)*s;
        if (ch < 0x80) {
            if (isalnum(ch) || ch == '_' || ch == '-' || ch == ' ' || ch == '.')
                tmp[n++] = (char)ch;
            s++;
        } else {
            s++;
            while ((*s & 0xC0) == 0x80)
                s++;
        }
        /* take(40) counts only the characters that passed the filter */
        count = (int)n;
    }
    tmp[n] = 0;
    for (i = 0; tmp[i] == '.'; i++) {
    }
    if (tmp[i] == 0)
        snprintf(out, size, "scan_");
    else
        snprintf(out, size, "%s", tmp + i);
}

static void handle_scan(client *c, const http_req *r)
{
    jv *v = parse_body(c, r), *frames, *sx;
    const char *mode_s, *pfx;
    double dpi;
    int mode, sixteen = 1, nj = 0;
    size_t i;
    advanced adv;
    char prefix[64], err[300];
    request *rq;
    char **used;

    if (v == NULL)
        return;
    frames = jget(v, "frames");
    mode_s = get_str(v, "mode");
    if (frames == NULL || frames->type != J_ARR || mode_s == NULL || (mode = mode_from_id(mode_s)) < 0 ||
        get_num(v, "dpi", &dpi) < 0 || get_advanced(v, &adv) < 0) {
        json_free(v);
        send_error(c, 422, "a scan needs frames, mode, dpi and optionally sixteen_bit, prefix and advanced");
        return;
    }
    sx = jget(v, "sixteen_bit");
    if (sx != NULL && sx->type == J_BOOL)
        sixteen = sx->b;
    pfx = get_str(v, "prefix");
    if (frames->count == 0) {
        json_free(v);
        send_error(c, 400, "Draw a frame on the prescan first");
        return;
    }
    if (!(dpi >= 20.0 && dpi <= 12000.0)) {
        json_free(v);
        send_error(c, 400, "Resolution must be 20 to 12000 dpi");
        return;
    }
    clean_prefix(pfx ? pfx : "", prefix, sizeof prefix);
    rq = xcalloc(1, sizeof *rq);
    rq->kind = REQ_SCAN;
    rq->jobs = xcalloc(frames->count, sizeof *rq->jobs);
    used = xcalloc(frames->count, sizeof *used);
    for (i = 0; i < frames->count; i++) {
        jv *f = frames->items[i], *al;
        const char *name = get_str(f, "name");
        frame_job *job = &rq->jobs[nj];
        cz_area area;
        cz_geometry g;
        if (name == NULL || get_area(jget(f, "area"), &area) < 0 ||
            adjust_from_json(jget(f, "adjustments"), &job->adj, err, sizeof err) < 0) {
            if (name == NULL || jget(f, "area") == NULL)
                snprintf(err, sizeof err, "each frame needs name, area and adjustments");
            rq->njobs = nj;
            request_free(rq);
            free(used);
            json_free(v);
            send_error(c, 422, err);
            return;
        }
        make_window(&job->window, area, dpi, mode, job->adj.film, &adv);
        g = window_geometry(&job->window);
        if (g.pixels_per_line > LIMITS->max_pixels_per_line || g.lines > 0xFFFF) {
            char msg[400], d[64];
            snprintf(d, sizeof d, "%g", dpi);
            snprintf(msg, sizeof msg, "%s is too large at %s dpi (%u x %u pixels). Lower the resolution.", name, d,
                     g.pixels_per_line, g.lines);
            rq->njobs = nj;
            request_free(rq);
            free(used);
            json_free(v);
            send_error(c, 400, msg);
            return;
        }
        al = jget(f, "auto_levels");
        job->name = xstrdup(name);
        job->auto_levels = al != NULL && al->type == J_BOOL && al->b;
        job->sixteen = sixteen;
        job->path = next_path(prefix, used, nj);
        used[nj] = job->path;
        nj++;
    }
    rq->njobs = nj;
    free(used);
    json_free(v);
    {
        char why[400];
        if (send_request(rq, why, sizeof why) != NULL) {
            send_error(c, 409, why);
        } else {
            sbuf b;
            sb_init(&b);
            sb_printf(&b, "{\"ok\":true,\"frames\":%d}", nj);
            send_json(c, 200, &b);
            sb_free(&b);
        }
    }
}

static int find_file_locked(uint64_t id)
{
    size_t i;
    for (i = 0; i < S.nfiles; i++)
        if (S.files[i].id == id)
            return (int)i;
    return -1;
}

static void handle_download(client *c, uint64_t id)
{
    char *path, *name, extra[600], msg[600];
    FILE *f;
    struct stat st;
    int i;
    size_t k;
    char buf[256 * 1024];

    lock();
    i = find_file_locked(id);
    if (i < 0) {
        unlock();
        send_error(c, 404, "No such file");
        return;
    }
    path = xstrdup(S.files[i].path);
    name = xstrdup(S.files[i].file_name);
    unlock();
    f = fopen(path, "rb");
    if (f == NULL || fstat(fileno(f), &st) != 0) {
        snprintf(msg, sizeof msg, "%s: %s", name, strerror(errno));
        if (f)
            fclose(f);
        free(path);
        free(name);
        send_error(c, 404, msg);
        return;
    }
    /* quotes removed so the header stays well formed */
    for (k = 0; name[k]; k++)
        if (name[k] == '"')
            memmove(name + k, name + k + 1, strlen(name + k)), k--;
    snprintf(extra, sizeof extra, "Content-Disposition: attachment; filename=\"%s\"\r\n", name);
    if (send_head(c, 200, "image/tiff", (uint64_t)st.st_size, extra) == 0) {
        size_t n;
        while ((n = fread(buf, 1, sizeof buf, f)) > 0)
            if (sock_write_full(c->s, buf, n) < 0) {
                c->keep_alive = 0;
                break;
            }
    }
    fclose(f);
    free(path);
    free(name);
}

static void handle_delete(client *c, uint64_t id)
{
    scan_file f;
    int i;
    lock();
    i = find_file_locked(id);
    if (i < 0) {
        unlock();
        send_error(c, 404, "No such file");
        return;
    }
    f = S.files[i];
    memmove(S.files + i, S.files + i + 1, (S.nfiles - (size_t)i - 1) * sizeof *S.files);
    S.nfiles--;
    unlock();
    if (remove(f.path) == 0)
        slog(0, "Removed %s from the server", f.file_name);
    else
        slog(1, "Could not remove %s: %s", f.file_name, strerror(errno));
    file_free(&f);
    send_ok(c);
}

/* ------------------------------------------------------------------ routing */

static void static_page(client *c, const char *type, const unsigned char *data, size_t len)
{
    send_body(c, 200, type, data, len, "Cache-Control: no-cache\r\n");
}

static int method_is(const http_req *r, const char *m)
{
    return strcmp(r->method, m) == 0;
}

static void route(client *c, const http_req *r)
{
    const char *p = r->path;
    int get = method_is(r, "GET") || method_is(r, "HEAD");
    int post = method_is(r, "POST");

    if (strcmp(p, "/") == 0 && get)
        return static_page(c, "text/html; charset=utf-8", ASSET_INDEX_HTML, sizeof ASSET_INDEX_HTML);
    if (strcmp(p, "/app.js") == 0 && get)
        return static_page(c, "text/javascript; charset=utf-8", ASSET_APP_JS, sizeof ASSET_APP_JS);
    if (strcmp(p, "/style.css") == 0 && get)
        return static_page(c, "text/css; charset=utf-8", ASSET_STYLE_CSS, sizeof ASSET_STYLE_CSS);
    if (strcmp(p, "/icon.png") == 0 && get)
        return static_page(c, "image/png", ASSET_ICON_PNG, sizeof ASSET_ICON_PNG);
    if (strncmp(p, "/api/", 5) != 0) {
        send_error(c, 404, "Not found");
        return;
    }
    p += 5;

    /* public */
    if (strcmp(p, "info") == 0 && get)
        return handle_info(c, r);
    if (strcmp(p, "login") == 0 && post)
        return handle_login(c, r);
    if (strcmp(p, "meta") == 0 && get)
        return handle_meta(c);

    if (!authorised(r)) {
        send_error(c, 401, "Access token needed");
        return;
    }
    if (strcmp(p, "state") == 0 && get)
        return handle_state(c, r);
    if (strcmp(p, "connect") == 0 && post)
        return handle_connect(c, r);
    if (strcmp(p, "disconnect") == 0 && post)
        return handle_simple(c, REQ_DISCONNECT);
    if (strcmp(p, "probe") == 0 && post)
        return handle_simple(c, REQ_PROBE);
    if (strcmp(p, "devices") == 0 && get)
        return handle_devices(c);
    if (strcmp(p, "cancel") == 0 && post)
        return handle_cancel(c);
    if (strcmp(p, "prescan") == 0 && post)
        return handle_prescan(c, r);
    if (strcmp(p, "prescan.png") == 0 && get)
        return handle_prescan_png(c);
    if (strcmp(p, "density") == 0 && get)
        return handle_density(c, r);
    if (strcmp(p, "preview") == 0 && post)
        return handle_preview(c, r);
    if (strcmp(p, "scan") == 0 && post)
        return handle_scan(c, r);
    if (strcmp(p, "presets") == 0 && get)
        return handle_presets(c);
    if (strcmp(p, "presets") == 0 && post)
        return handle_save_preset(c, r);
    if (strncmp(p, "files/", 6) == 0) {
        char *end;
        unsigned long long id = strtoull(p + 6, &end, 10);
        if (p[6] == 0 || *end != 0) {
            send_error(c, 400, "Invalid URL: file id must be a number");
            return;
        }
        if (get)
            return handle_download(c, id);
        if (method_is(r, "DELETE"))
            return handle_delete(c, id);
    }
    send_error(c, 405, "Method not allowed");
}

static void *client_thread(void *arg)
{
    client *c = arg;
    sbuf in;
    sb_init(&in);
    sock_timeouts(c->s, 120000, 120000);
    sock_nodelay(c->s);
    for (;;) {
        http_req r;
        int st = read_request(c, &r, &in);
        if (st != 0) {
            if (st > 0) {
                c->keep_alive = 0;
                send_error(c, st, st == 413 ? "Request too large" : "Bad request");
            }
            request_done(&r);
            break;
        }
        route(c, &r);
        request_done(&r);
        if (!c->keep_alive)
            break;
    }
    sb_free(&in);
    sock_close(c->s);
    free(c);
    lock();
    S.connections--;
    unlock();
    return NULL;
}

/* ------------------------------------------------------------------ main */

static sock_t listen_on(const char *addr)
{
    char host[256], port[16];
    const char *colon = strrchr(addr, ':');
    struct addrinfo hints, *res;
    sock_t s;
    int one = 1;

    if (colon == NULL) {
        fprintf(stderr, "--listen needs HOST:PORT, for example 0.0.0.0:%d\n", CZ_DEFAULT_PORT);
        return BAD_SOCK;
    }
    snprintf(host, sizeof host, "%.*s", (int)(colon - addr), addr);
    snprintf(port, sizeof port, "%s", colon + 1);
    if (host[0] == '[' && host[strlen(host) - 1] == ']') {
        memmove(host, host + 1, strlen(host));
        host[strlen(host) - 1] = 0;
    }
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_PASSIVE;
    if (getaddrinfo(host[0] ? host : NULL, port, &hints, &res) != 0) {
        fprintf(stderr, "cannot listen on %s: unknown address\n", addr);
        return BAD_SOCK;
    }
    s = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (s == BAD_SOCK || setsockopt(s, SOL_SOCKET, SO_REUSEADDR, (const char *)&one, sizeof one) != 0 ||
        bind(s, res->ai_addr, (int)res->ai_addrlen) != 0 || listen(s, 16) != 0) {
        fprintf(stderr, "cannot listen on %s: %s\n", addr, strerror(sock_errno()));
        freeaddrinfo(res);
        if (s != BAD_SOCK)
            sock_close(s);
        return BAD_SOCK;
    }
    freeaddrinfo(res);
    return s;
}

static void usage(void)
{
    printf("cezanne-server (C) %s: serves the Cezanne Scan interface to browsers on your network\n"
           "\n"
           "Usage: cezanne-server [--listen HOST:PORT] [--token SECRET] [--output-dir DIR]\n"
           "                      [--connect auto|iokit:ID|bridge:HOST] [--bridge-token SECRET]\n"
           "       cezanne-server --list\n"
           "\n"
           "  --listen HOST:PORT    address to listen on (default 0.0.0.0:%d, every network interface)\n"
           "  --token SECRET        access token browsers must enter once, also read from\n"
           "                        CEZANNE_SERVER_TOKEN. Recommended on a shared network\n"
           "  --output-dir DIR      folder for finished scans (default ~/Pictures/Cezanne Scans)\n"
           "  --connect WHAT        connect at start up: auto (first scanner on this Mac), a device\n"
           "                        from --list such as iokit:4294968123, or bridge:HOST for a\n"
           "                        cezanne-bridge on another computer\n"
           "  --bridge-token SECRET token for --connect bridge:HOST, also read from CEZANNE_BRIDGE_TOKEN\n"
           "  --list                list the SCSI devices this Mac can reach and exit\n"
           "  --version             print the version\n",
           CZ_VERSION, CZ_DEFAULT_PORT);
}

/* fill the lookup tables shared by every thread before any thread starts */
static void warm_up(void)
{
    cz_processed p;
    uint16_t px[3] = { 0, 0, 0 };
    uint8_t *rgba;
    sbuf png;
    memset(&p, 0, sizeof p);
    p.width = p.height = 1;
    p.channels = 3;
    p.data = px;
    rgba = processed_to_rgba8(&p);
    sb_init(&png);
    png_encode_rgba(1, 1, rgba, &png);
    sb_free(&png);
    free(rgba);
}

int main(int argc, char **argv)
{
    const char *listen_addr = NULL, *token = getenv("CEZANNE_SERVER_TOKEN"), *outdir = NULL, *conn = NULL;
    const char *bridge_token = getenv("CEZANNE_BRIDGE_TOKEN");
    char default_listen[32], *dflt_out = NULL;
    pthread_t th;
    pthread_attr_t attr;
    sock_t ls;
    int i;

    for (i = 1; i < argc; i++) {
        const char *a = argv[i], *next = i + 1 < argc ? argv[i + 1] : NULL;
        if (strcmp(a, "--help") == 0 || strcmp(a, "-h") == 0) {
            usage();
            return 0;
        } else if (strcmp(a, "--version") == 0 || strcmp(a, "-V") == 0) {
            printf("cezanne-server (C) %s\n", CZ_VERSION);
            return 0;
        } else if (strcmp(a, "--list") == 0) {
            return cz_list_all_devices(stdout) < 0 ? 1 : 0;
        } else if (strcmp(a, "--listen") == 0 && next) {
            listen_addr = next, i++;
        } else if (strcmp(a, "--token") == 0 && next) {
            token = next, i++;
        } else if (strcmp(a, "--output-dir") == 0 && next) {
            outdir = next, i++;
        } else if (strcmp(a, "--connect") == 0 && next) {
            conn = next, i++;
        } else if (strcmp(a, "--bridge-token") == 0 && next) {
            bridge_token = next, i++;
        } else {
            fprintf(stderr, "unknown or incomplete option %s, see --help\n", a);
            return 2;
        }
    }
    snprintf(default_listen, sizeof default_listen, "0.0.0.0:%d", CZ_DEFAULT_PORT);
    if (listen_addr == NULL)
        listen_addr = default_listen;
    if (bridge_token == NULL)
        bridge_token = "";
    if (sock_init() != 0) {
        fprintf(stderr, "cannot start networking\n");
        return 1;
    }

    memset(&S, 0, sizeof S);
    pthread_mutex_init(&S.lock, NULL);
    pthread_cond_init(&S.wake, NULL);
    if (token != NULL) {
        char *t = xstrdup(token);
        trim_in_place(t);
        if (t[0])
            S.token = t;
        else
            free(t);
    }
    if (S.token == NULL && strncmp(listen_addr, "127.", 4) != 0 && strncmp(listen_addr, "localhost", 9) != 0)
        log_warn("no --token set: anyone on your network can use the scanner from a browser");
    if (outdir == NULL) {
        const char *home = getenv("HOME");
#ifdef _WIN32
        if (home == NULL)
            home = getenv("USERPROFILE");
#endif
        dflt_out = xmalloc(strlen(home ? home : ".") + 40);
        sprintf(dflt_out, "%s%cPictures%cCezanne Scans", home ? home : ".", PATH_SEP, PATH_SEP);
        outdir = dflt_out;
    }
    S.output_dir = xstrdup(outdir);
    free(dflt_out);
    if (make_dirs(S.output_dir) < 0) {
        fprintf(stderr, "cannot create %s: %s\n", S.output_dir, strerror(errno));
        return 1;
    }
    if (gethostname(S.host, sizeof S.host) != 0)
        S.host[0] = 0;
    S.host[sizeof S.host - 1] = 0;
    warm_up();
    load_existing_files();

    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    pthread_attr_setstacksize(&attr, 1u << 20);
    if (pthread_create(&th, &attr, scanner_thread, NULL) != 0) {
        fprintf(stderr, "cannot start the scanner thread\n");
        return 1;
    }

    if (conn != NULL) {
        request *rq = xcalloc(1, sizeof *rq);
        char *cs = xstrdup(conn), why[300];
        trim_in_place(cs);
        rq->kind = REQ_CONNECT;
        if (strcmp(cs, "sim") == 0 || strcmp(cs, "simulator") == 0) {
            rq->conn = CONN_SIM;
            S.start_kind = "Simulator";
        } else if (strcmp(cs, "auto") == 0 || cs[0] == 0) {
            rq->conn = CONN_AUTO;
            S.start_kind = "Local";
        } else if (strncmp(cs, "bridge:", 7) == 0) {
            rq->conn = CONN_BRIDGE;
            rq->address = xstrdup(cs + 7);
            rq->token = xstrdup(bridge_token);
            S.start_kind = "Bridge";
        } else {
            rq->conn = CONN_LOCAL;
            rq->address = xstrdup(cs);
            S.start_kind = "Local";
        }
        free(cs);
        send_request(rq, why, sizeof why);
    }

    ls = listen_on(listen_addr);
    if (ls == BAD_SOCK)
        return 1;
    log_info("cezanne-server (C) %s listening on http://%s", CZ_VERSION, listen_addr);
    log_info("finished scans are kept in %s", S.output_dir);

    for (;;) {
        struct sockaddr_storage peer;
        socklen_t plen = sizeof peer;
        sock_t cs = accept(ls, (struct sockaddr *)&peer, &plen);
        client *c;
        int too_many;
        if (cs == BAD_SOCK) {
            sleep_ms(50);
            continue;
        }
        lock();
        too_many = S.connections >= MAX_CONNECTIONS;
        if (!too_many)
            S.connections++;
        unlock();
        if (too_many) {
            sock_close(cs);
            continue;
        }
        c = xcalloc(1, sizeof *c);
        c->s = cs;
        if (pthread_create(&th, &attr, client_thread, c) != 0) {
            sock_close(cs);
            free(c);
            lock();
            S.connections--;
            unlock();
        }
    }
}
