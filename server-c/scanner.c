/*
 * The FT-S5000 protocol and driver: crates/cezanne-core/src/protocol (window.rs, cdb.rs,
 * inquiry.rs) and scanner.rs. See docs/protocol.md for where every byte comes from.
 */
#include "cz.h"

#include <math.h>
#include <stdlib.h>
#include <string.h>

#define SHORT_MS 10000u
#define LONG_MS 120000u
#define READ_CHUNK (256u * 1024u)
#define DESCRIPTOR_LEN 176
#define HEADER_LEN 8

/* ------------------------------------------------------------------ enums */

static const char *const MODE_IDS[] = { "Gray8", "Gray16", "Rgb24", "Rgb48" };
static const char *const MODE_LABELS[] = { "Grey 8 bit", "Grey 16 bit", "Colour 24 bit", "Colour 48 bit" };

const char *mode_id(int m) { return MODE_IDS[m]; }
const char *mode_label(int m) { return MODE_LABELS[m]; }
int mode_channels(int m) { return m == MODE_GRAY8 || m == MODE_GRAY16 ? 1 : 3; }
int mode_bytes_per_sample(int m) { return m == MODE_GRAY8 || m == MODE_RGB24 ? 1 : 2; }
static int mode_composition(int m) { return mode_channels(m) == 1 ? 2 : 5; }
static int mode_bits(int m) { return m == MODE_GRAY8 ? 8 : m == MODE_GRAY16 ? 16 : m == MODE_RGB24 ? 24 : 48; }

int mode_from_id(const char *s)
{
    int i;
    for (i = 0; i < 4; i++)
        if (strcmp(s, MODE_IDS[i]) == 0)
            return i;
    return -1;
}

static int mode_from_codes(int composition, int bits)
{
    if (composition == 2 && bits == 8)
        return MODE_GRAY8;
    if (composition == 2 && bits == 16)
        return MODE_GRAY16;
    if (composition == 5 && bits == 24)
        return MODE_RGB24;
    if (composition == 5 && bits == 48)
        return MODE_RGB48;
    return -1;
}

const char *original_label(int o)
{
    switch (o) {
    case ORI_POSITIVE_FILM: return "Positive film";
    case ORI_NEGATIVE_FILM: return "Negative film";
    case ORI_PHOTOGRAPH: return "Photograph";
    case ORI_PRINTED: return "Printed material";
    default: return "Watercolor";
    }
}

int original_is_reflective(int o)
{
    return !(o == ORI_POSITIVE_FILM || o == ORI_NEGATIVE_FILM);
}

/* the driver's lookup table selector (STR# 2000), packed like SetSCSIWindowBasicLUTID */
static void original_lut_bytes(int o, int gray, uint8_t out[4])
{
    const char *code;
    int d[5], i;
    if (o == ORI_POSITIVE_FILM)
        code = gray ? "11006" : "11003";
    else if (o == ORI_NEGATIVE_FILM)
        code = "11005";
    else
        code = "22004";
    for (i = 0; i < 5; i++)
        d[i] = code[i] - '0';
    out[0] = (uint8_t)d[2];
    out[1] = (uint8_t)d[0];
    out[2] = (uint8_t)d[4];
    out[3] = (uint8_t)((d[3] << 4) | d[1]);
}

double unit_per_mm(int u)
{
    return u == UNIT_INCH1200 ? 1200.0 / 25.4 : 10.0;
}

const char *unit_id(int u)
{
    return u == UNIT_INCH1200 ? "Inch1200" : "TenthMm";
}

int unit_from_id(const char *s)
{
    if (strcmp(s, "TenthMm") == 0)
        return UNIT_TENTH_MM;
    if (strcmp(s, "Inch1200") == 0)
        return UNIT_INCH1200;
    return -1;
}

const cz_limits LIMITS_FT_S5000 = { 3420, 5310, 12000, 200, 120000 };
const cz_limits LIMITS_FT_S3000 = { 3420, 5310, 12000, 200, 66000 };

void limits_bed_mm(const cz_limits *l, int unit, double *w, double *h)
{
    *w = (double)l->max_x / unit_per_mm(unit);
    *h = (double)l->max_y / unit_per_mm(unit);
}

/* ------------------------------------------------------------------ windows */

void window_init(cz_window *w, cz_area area, double dpi, int mode)
{
    memset(w, 0, sizeof *w);
    w->window_id = 1;
    w->area = area;
    w->x_dpi = dpi;
    w->y_dpi = dpi;
    w->mode = mode;
    w->original = ORI_POSITIVE_FILM;
    w->preview = 0;
    w->area_unit = UNIT_TENTH_MM;
    w->resolution_scale = 10;
}

/* Rust `f as i64` and `f as u32` saturate, so clamp before converting */
static int64_t sat_i64(double v)
{
    if (v != v)
        return 0;
    if (v >= 9.2233720368547758e18)
        return INT64_MAX;
    if (v <= -9.2233720368547758e18)
        return INT64_MIN;
    return (int64_t)v;
}

static uint32_t sat_u32(double v)
{
    if (v != v || v <= 0)
        return 0;
    if (v >= 4294967295.0)
        return 0xFFFFFFFFu;
    return (uint32_t)v;
}

static uint32_t px_count(double mm, double dpi)
{
    int64_t n = sat_i64(floor(mm / 25.4 * dpi + 1e-6));
    if (n < 1)
        n = 1;
    return (uint32_t)n; /* `as u32` of an i64 wraps, which the limits checks rule out first */
}

cz_geometry window_geometry(const cz_window *w)
{
    cz_geometry g;
    g.pixels_per_line = px_count(w->area.width_mm, w->x_dpi);
    g.lines = px_count(w->area.height_mm, w->y_dpi);
    g.mode = w->mode;
    return g;
}

size_t geometry_bytes_per_line(const cz_geometry *g)
{
    return (size_t)g->pixels_per_line * (size_t)(mode_channels(g->mode) * mode_bytes_per_sample(g->mode));
}

size_t geometry_total_bytes(const cz_geometry *g)
{
    return geometry_bytes_per_line(g) * g->lines;
}

static void put16(uint8_t *d, size_t at, uint16_t v)
{
    d[at] = (uint8_t)(v >> 8);
    d[at + 1] = (uint8_t)v;
}

static void put32(uint8_t *d, size_t at, uint32_t v)
{
    d[at] = (uint8_t)(v >> 24);
    d[at + 1] = (uint8_t)(v >> 16);
    d[at + 2] = (uint8_t)(v >> 8);
    d[at + 3] = (uint8_t)v;
}

static uint16_t be16(const uint8_t *b) { return (uint16_t)((b[0] << 8) | b[1]); }
static uint32_t be32(const uint8_t *b)
{
    return ((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) | ((uint32_t)b[2] << 8) | b[3];
}

/* WindowDescriptor::build: the same bytes as SCREEN's driver */
static int descriptor_build(const cz_window *s, uint8_t d[DESCRIPTOR_LEN], cz_geometry *g, cz_err *e)
{
    double u;
    uint8_t lut[4];
    int reflective;

    if (s->x_dpi <= 0.0 || s->y_dpi <= 0.0)
        return cz_fail(e, E_INVALID, "resolution must be positive");
    *g = window_geometry(s);
    memset(d, 0, DESCRIPTOR_LEN);
    u = unit_per_mm(s->area_unit);
    d[0] = s->window_id;
    put32(d, 6, sat_u32(fmax(round(s->area.x_mm * u), 0.0)));
    put32(d, 10, sat_u32(fmax(round(s->area.y_mm * u), 0.0)));
    d[41] = 2;
    d[42] = 100;
    d[43] = 0x20;
    d[45] = s->preview ? 1 : 2;
    if (g->pixels_per_line > 0xFFFF)
        return cz_fail(e, E_INVALID, "pixels per line does not fit in 16 bits, lower the resolution or the area");
    if (g->lines > 0xFFFF)
        return cz_fail(e, E_INVALID, "line count does not fit in 16 bits, lower the resolution or the area");
    put16(d, 46, (uint16_t)g->pixels_per_line);
    put16(d, 48, (uint16_t)g->lines);
    d[50] = (uint8_t)mode_composition(s->mode);
    d[51] = (uint8_t)mode_bits(s->mode);
    put32(d, 54, sat_u32(round(s->x_dpi * (double)s->resolution_scale)));
    put32(d, 58, sat_u32(round(s->y_dpi * (double)s->resolution_scale)));
    original_lut_bytes(s->original, mode_channels(s->mode) == 1, lut);
    memcpy(d + 62, lut, 4);
    reflective = original_is_reflective(s->original);
    d[142] = reflective ? 2 : 0;
    d[144] = (uint8_t)reflective;
    return 0;
}

static uint32_t extent_tenth_mm(uint32_t pixels, uint32_t res)
{
    if (res == 0)
        return 0xFFFFFFFFu;
    return (uint32_t)(((uint64_t)pixels * 2540 + res - 1) / res);
}

static uint32_t sat_add(uint32_t a, uint32_t b)
{
    return a > 0xFFFFFFFFu - b ? 0xFFFFFFFFu : a + b;
}

/* the checks the firmware runs before it accepts a window */
static int descriptor_validate(const uint8_t *d, const cz_limits *l, cz_err *e)
{
    uint32_t xr = be32(d + 54), yr = be32(d + 58);
    uint16_t ppl = be16(d + 46), lines = be16(d + 48);
    uint32_t x_end, y_end;

    if (xr < l->resolution_min || xr > l->resolution_max)
        return cz_fail(e, E_INVALID, "X resolution field %u outside %u..=%u", xr, l->resolution_min, l->resolution_max);
    if (yr < l->resolution_min || yr > l->resolution_max)
        return cz_fail(e, E_INVALID, "Y resolution field %u outside %u..=%u", yr, l->resolution_min, l->resolution_max);
    if (ppl > l->max_pixels_per_line)
        return cz_fail(e, E_INVALID, "%u pixels per line is above the limit of %u", ppl, l->max_pixels_per_line);
    x_end = sat_add(be32(d + 6), extent_tenth_mm(ppl, xr));
    if (x_end > l->max_x)
        return cz_fail(e, E_INVALID, "area runs past the bed width (%u > %u)", x_end, l->max_x);
    y_end = sat_add(be32(d + 10), extent_tenth_mm(lines, yr));
    if (y_end > l->max_y)
        return cz_fail(e, E_INVALID, "area runs past the bed length (%u > %u)", y_end, l->max_y);
    if (mode_from_codes(d[50], d[51]) < 0)
        return cz_fail(e, E_INVALID, "unsupported composition %u with %u bits", d[50], d[51]);
    return 0;
}

/* ------------------------------------------------------------------ CDBs */

static void put_be24(uint8_t *b, uint32_t v)
{
    b[0] = (uint8_t)(v >> 16);
    b[1] = (uint8_t)(v >> 8);
    b[2] = (uint8_t)v;
}

static void cdb10(uint8_t c[10], uint8_t op, uint32_t len)
{
    memset(c, 0, 10);
    c[0] = op;
    put_be24(c + 6, len);
}

/* ------------------------------------------------------------------ driver */

static int run(cz_scanner *s, const uint8_t *cdb, size_t n, int dir, uint8_t *buf, size_t len, unsigned ms,
               size_t *done, cz_err *e)
{
    return cz_scsi_run(s->t, cdb, n, dir, buf, len, ms, done, e);
}

static int is_supported(const char *vendor, const char *product)
{
    return strcmp(vendor, "SCREEN") == 0 && (strncmp(product, "FT-S5", 5) == 0 || strncmp(product, "FT-S3", 5) == 0);
}

/* inquiry text: lossy UTF-8 in Rust, the scanner only sends ASCII */
static void inquiry_text(const uint8_t *b, size_t n, char *out)
{
    size_t i;
    for (i = 0; i < n; i++)
        out[i] = (b[i] >= 0x20 && b[i] < 0x7F) ? (char)b[i] : '?';
    out[n] = 0;
    while (n > 0 && (out[n - 1] == ' ' || out[n - 1] == '\t' || out[n - 1] == '\n' || out[n - 1] == '\r'))
        out[--n] = 0;
}

cz_scanner *scanner_open(cz_transport *t, cz_err *e)
{
    uint8_t buf[36], tur[6] = { 0, 0, 0, 0, 0, 0 };
    uint8_t inq[6] = { 0x12, 0, 0, 0, 36, 0 };
    size_t n = 0;
    cz_scanner *s;
    int i;

    if (cz_scsi_run(t, inq, 6, DIR_IN, buf, sizeof buf, SHORT_MS, &n, e) < 0) {
        t->close(t);
        return NULL;
    }
    s = xcalloc(1, sizeof *s);
    s->t = t;
    inquiry_text(buf + 8, 8, s->vendor);
    inquiry_text(buf + 16, 16, s->product);
    inquiry_text(buf + 32, 4, s->revision);
    s->vendor_specific_len = 0; /* only 36 bytes are asked for */
    if (!is_supported(s->vendor, s->product)) {
        cz_fail(e, E_NOT_FOUND, "%s %s is not a supported SCREEN scanner", s->vendor, s->product);
        scanner_close(s);
        return NULL;
    }
    s->limits = strncmp(s->product, "FT-S3", 5) == 0 ? LIMITS_FT_S3000 : LIMITS_FT_S5000;
    log_info("opened %s %s rev %s via %s", s->vendor, s->product, s->revision, t->desc);
    /* consume a pending power on UNIT ATTENTION so the next real command does not fail */
    for (i = 0; i < 2; i++) {
        cz_err te;
        if (run(s, tur, 6, DIR_NONE, NULL, 0, SHORT_MS, NULL, &te) < 0 && te.kind == E_SCSI && te.has_sense &&
            te.sense_key == SK_UNIT_ATTENTION)
            continue;
        break;
    }
    return s;
}

static int release_unit(cz_scanner *s, cz_err *e)
{
    uint8_t c[6] = { 0x17, 0, 0, 0, 0, 0 };
    s->reserved = 0;
    return run(s, c, 6, DIR_NONE, NULL, 0, SHORT_MS, NULL, e);
}

void scanner_close(cz_scanner *s)
{
    if (s == NULL)
        return;
    if (s->reserved) {
        cz_err e;
        release_unit(s, &e);
    }
    s->t->close(s->t);
    free(s);
}

void scanner_describe(const cz_scanner *s, char *out, size_t size)
{
    snprintf(out, size, "%s %s %s (%s)", s->vendor, s->product, s->revision, s->t->desc);
}

/* poll TEST UNIT READY until the scanner is ready (lamp warm up can take minutes) */
static int wait_ready(cz_scanner *s, unsigned max_s, volatile int *cancel, cz_err *e)
{
    uint8_t tur[6] = { 0, 0, 0, 0, 0, 0 };
    double start = now_seconds();
    for (;;) {
        if (*cancel)
            return cz_fail(e, E_CANCELLED, "");
        if (run(s, tur, 6, DIR_NONE, NULL, 0, SHORT_MS, NULL, e) == 0)
            return 0;
        if (e->kind == E_SCSI && e->has_sense && e->sense_key == SK_UNIT_ATTENTION)
            continue;
        if (e->kind == E_SCSI && e->has_sense && e->sense_key == SK_NOT_READY) {
            if (now_seconds() - start > max_s) {
                char st[160];
                cz_sense_text(e->sense_key, e->asc, e->ascq, st, sizeof st);
                return cz_fail(e, E_TIMEOUT, "scanner not ready after %us: %s", max_s, st);
            }
            sleep_ms(500);
            continue;
        }
        return -1;
    }
}

static int reserve_unit(cz_scanner *s, cz_err *e)
{
    uint8_t c[6] = { 0x16, 0, 0, 0, 0, 0 };
    if (run(s, c, 6, DIR_NONE, NULL, 0, SHORT_MS, NULL, e) < 0)
        return -1;
    s->reserved = 1;
    return 0;
}

static int set_window(cz_scanner *s, const uint8_t *desc, cz_err *e)
{
    uint8_t p[HEADER_LEN + DESCRIPTOR_LEN], c[10];
    if (descriptor_validate(desc, &s->limits, e) < 0)
        return -1;
    memset(p, 0, HEADER_LEN);
    put16(p, 0, DESCRIPTOR_LEN + 6);
    put16(p, 6, DESCRIPTOR_LEN);
    memcpy(p + HEADER_LEN, desc, DESCRIPTOR_LEN);
    cdb10(c, 0x24, sizeof p);
    return run(s, c, 10, DIR_OUT, p, sizeof p, SHORT_MS, NULL, e);
}

/* SCAN, waiting out NOT READY (lamp warm up, 04/80) for up to five minutes like SCREEN's driver */
static int start_scan(cz_scanner *s, uint8_t id, volatile int *cancel, cz_err *e)
{
    uint8_t c[6] = { 0x1B, 0, 0, 0, 1, 0 };
    double begin = now_seconds();
    for (;;) {
        if (run(s, c, 6, DIR_OUT, &id, 1, LONG_MS, NULL, e) == 0)
            return 0;
        if (e->kind == E_SCSI && e->has_sense && e->sense_key == SK_NOT_READY) {
            char st[160];
            cz_sense_text(e->sense_key, e->asc, e->ascq, st, sizeof st);
            if (now_seconds() - begin > 300)
                return cz_fail(e, E_TIMEOUT, "scanner still not ready after five minutes: %s", st);
            if (*cancel)
                return cz_fail(e, E_CANCELLED, "");
            log_info("scanner warming up (%s), waiting", st);
            sleep_ms(2000);
            continue;
        }
        return -1;
    }
}

static uint8_t *read_image(cz_scanner *s, const cz_geometry *g, cz_progress_fn progress, void *ctx,
                           volatile int *cancel, cz_err *e)
{
    size_t total = geometry_total_bytes(g);
    size_t bpl = geometry_bytes_per_line(g);
    size_t chunk, done = 0;
    uint8_t *out;

    if (bpl < 1)
        bpl = 1;
    chunk = READ_CHUNK / bpl;
    if (chunk < 1)
        chunk = 1;
    chunk *= bpl;
    out = xcalloc(total ? total : 1, 1);
    while (done < total) {
        uint8_t c[10];
        size_t want = chunk < total - done ? chunk : total - done, n = 0;
        if (*cancel) {
            free(out);
            cz_fail(e, E_CANCELLED, "");
            return NULL;
        }
        cdb10(c, 0x28, (uint32_t)want);
        if (run(s, c, 10, DIR_IN, out + done, want, LONG_MS, &n, e) < 0) {
            free(out);
            return NULL;
        }
        if (n == 0) {
            free(out);
            cz_fail(e, E_PROTOCOL, "scanner stopped sending after %lu of %lu bytes", (unsigned long)done,
                    (unsigned long)total);
            return NULL;
        }
        done += n;
        progress(ctx, (float)done / (float)total);
    }
    return out;
}

cz_raw *scanner_scan(cz_scanner *s, const cz_window *w, cz_progress_fn progress, void *ctx, volatile int *cancel,
                     cz_err *e)
{
    uint8_t desc[DESCRIPTOR_LEN];
    cz_geometry g;
    cz_raw *raw = NULL;
    uint8_t *bytes;
    int reserved_here;

    if (descriptor_build(w, desc, &g, e) < 0)
        return NULL;
    if (wait_ready(s, 300, cancel, e) < 0)
        return NULL;
    reserved_here = !s->reserved;
    if (reserved_here && reserve_unit(s, e) < 0)
        return NULL;
    if (set_window(s, desc, e) == 0 && start_scan(s, desc[0], cancel, e) == 0) {
        bytes = read_image(s, &g, progress, ctx, cancel, e);
        if (bytes != NULL) {
            raw = raw_from_scanner(&g, bytes, geometry_total_bytes(&g), w->x_dpi, w->y_dpi, e);
            free(bytes);
        }
    }
    if (reserved_here) {
        cz_err re;
        if (release_unit(s, &re) < 0)
            log_warn("RELEASE UNIT failed: %s", re.msg);
    }
    return raw;
}

cz_raw *scanner_preview(cz_scanner *s, const cz_window *w, cz_progress_fn progress, void *ctx,
                        volatile int *cancel, cz_err *e)
{
    cz_window p = *w;
    p.preview = 1;
    return scanner_scan(s, &p, progress, ctx, cancel, e);
}

/* ------------------------------------------------------------------ probe */

/* Rust's {:02x?} of a byte slice: [0a, ff] */
static void hex_list(sbuf *b, const uint8_t *p, size_t n)
{
    size_t i;
    sb_byte(b, '[');
    for (i = 0; i < n; i++)
        sb_printf(b, "%s%02x", i ? ", " : "", p[i]);
    sb_byte(b, ']');
}

static void describe_window(sbuf *b, const uint8_t *d)
{
    size_t i;
    sb_printf(b, "WindowDescriptor { id: %u, ulx: %u, uly: %u, pixels_per_line: %u, lines: %u, composition: %u, "
                 "bits: %u, x_res: %u, y_res: %u, mode: %u, lut: [%u, %u, %u, %u], light: %u }",
              d[0], be32(d + 6), be32(d + 10), be16(d + 46), be16(d + 48), d[50], d[51], be32(d + 54), be32(d + 58),
              d[45], d[62], d[63], d[64], d[65], d[144]);
    sb_byte(b, '\n');
    for (i = 0; i < DESCRIPTOR_LEN; i += 16) {
        size_t k;
        if (i)
            sb_byte(b, '\n');
        sb_printf(b, "%3lu:", (unsigned long)i);
        for (k = i; k < i + 16 && k < DESCRIPTOR_LEN; k++)
            sb_printf(b, " %02x", d[k]);
    }
}

char *scanner_probe(cz_scanner *s, cz_err *e)
{
    sbuf r;
    char dev[400];
    uint8_t page[64], st[6], c6[6] = { 0x12, 1, 0, 0, 64, 0 }, c10[10];
    size_t n = 0;
    cz_err pe;
    uint8_t id;

    (void)e;
    sb_init(&r);
    scanner_describe(s, dev, sizeof dev);
    sb_printf(&r, "Device: %s\n", dev);
    sb_puts(&r, "Inquiry vendor bytes: ");
    hex_list(&r, s->vendor_specific, s->vendor_specific_len);
    sb_byte(&r, '\n');
    if (run(s, c6, 6, DIR_IN, page, sizeof page, SHORT_MS, &n, &pe) == 0) {
        sb_puts(&r, "Vendor inquiry page: ");
        hex_list(&r, page, n);
        sb_byte(&r, '\n');
    } else {
        sb_printf(&r, "Vendor inquiry page: %s\n", pe.msg);
    }
    cdb10(c10, 0xCE, 6);
    if (run(s, c10, 10, DIR_IN, st, 6, SHORT_MS, NULL, &pe) == 0) {
        sb_puts(&r, "Vendor status (0xCE): ");
        hex_list(&r, st, 6);
        sb_byte(&r, '\n');
    } else {
        sb_printf(&r, "Vendor status (0xCE): %s\n", pe.msg);
    }
    for (id = 0; id < 4; id++) {
        uint8_t b[HEADER_LEN + DESCRIPTOR_LEN];
        cdb10(c10, 0x25, sizeof b);
        c10[5] = id;
        if (run(s, c10, 10, DIR_IN, b, sizeof b, SHORT_MS, &n, &pe) == 0 && n < sizeof b)
            cz_fail(&pe, E_PROTOCOL, "GET WINDOW returned %lu bytes", (unsigned long)n), n = 0;
        if (n >= sizeof b) {
            sb_printf(&r, "\nWindow %u: ", id);
            describe_window(&r, b + HEADER_LEN);
            sb_byte(&r, '\n');
        } else {
            sb_printf(&r, "\nWindow %u: %s\n", id, pe.msg);
        }
        n = 0;
    }
    return (char *)(sb_str(&r), r.p);
}
