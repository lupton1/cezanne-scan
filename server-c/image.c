/*
 * Images: crates/cezanne-core/src/image (mod.rs, pipeline.rs, icc.rs, tiff.rs, png.rs).
 *
 * The arithmetic follows the Rust code step by step and in the same precision (the
 * adjustment values are f32 there, so they are float here), so the saved files match.
 */
#include "cz.h"

#include <errno.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>

/* Rust `as u16` / `as u8` / `as u32` saturate and send NaN to 0 */
static uint16_t sat_u16(double v)
{
    if (!(v > 0))
        return 0;
    if (v >= 65535.0)
        return 65535;
    return (uint16_t)v;
}

static uint8_t sat_u8(double v)
{
    if (!(v > 0))
        return 0;
    if (v >= 255.0)
        return 255;
    return (uint8_t)v;
}

static uint32_t sat_u32(double v)
{
    if (!(v > 0))
        return 0;
    if (v >= 4294967295.0)
        return 0xFFFFFFFFu;
    return (uint32_t)v;
}

static double clampd(double v, double lo, double hi)
{
    /* f64::clamp, NaN stays NaN */
    if (v < lo)
        return lo;
    if (v > hi)
        return hi;
    return v;
}

static float clampf(float v, float lo, float hi)
{
    if (v < lo)
        return lo;
    if (v > hi)
        return hi;
    return v;
}

/* ------------------------------------------------------------------ raw images */

cz_raw *raw_from_scanner(const cz_geometry *g, const uint8_t *bytes, size_t len, double x_dpi, double y_dpi,
                         cz_err *e)
{
    size_t total = geometry_total_bytes(g), n, i;
    int channels = mode_channels(g->mode);
    cz_raw *r;

    if (len < total) {
        cz_fail(e, E_PROTOCOL, "expected %lu image bytes, got %lu", (unsigned long)total, (unsigned long)len);
        return NULL;
    }
    n = (size_t)g->pixels_per_line * g->lines * (size_t)channels;
    r = xcalloc(1, sizeof *r);
    r->width = g->pixels_per_line;
    r->height = g->lines;
    r->channels = channels;
    r->x_dpi = x_dpi;
    r->y_dpi = y_dpi;
    r->source_bits = mode_bytes_per_sample(g->mode) * 8;
    r->data = xmalloc(n * sizeof *r->data);
    if (mode_bytes_per_sample(g->mode) == 2) {
        for (i = 0; i < n; i++)
            r->data[i] = (uint16_t)((bytes[2 * i] << 8) | bytes[2 * i + 1]);
    } else {
        /* 8 bit modes arrive gamma encoded, undo it so the pipeline always sees linear light */
        uint16_t lut[256];
        int v;
        for (v = 0; v < 256; v++)
            lut[v] = sat_u16(round(pow((double)v / 255.0, 2.2) * 65535.0));
        for (i = 0; i < n; i++)
            r->data[i] = lut[bytes[i]];
    }
    return r;
}

void raw_free(cz_raw *r)
{
    if (r != NULL) {
        free(r->data);
        free(r);
    }
}

cz_raw *raw_crop(const cz_raw *r, uint32_t x0, uint32_t y0, uint32_t w, uint32_t h)
{
    cz_raw *o;
    uint32_t y;
    size_t row;

    if (x0 > (r->width ? r->width - 1 : 0))
        x0 = r->width ? r->width - 1 : 0;
    if (y0 > (r->height ? r->height - 1 : 0))
        y0 = r->height ? r->height - 1 : 0;
    if (w > r->width - x0)
        w = r->width - x0;
    if (w < 1)
        w = 1;
    if (h > r->height - y0)
        h = r->height - y0;
    if (h < 1)
        h = 1;
    o = xcalloc(1, sizeof *o);
    *o = *r;
    o->width = w;
    o->height = h;
    row = (size_t)w * (size_t)r->channels;
    o->data = xmalloc((size_t)h * row * sizeof *o->data);
    for (y = 0; y < h; y++) {
        size_t start = ((size_t)(y0 + y) * r->width + x0) * (size_t)r->channels;
        memcpy(o->data + (size_t)y * row, r->data + start, row * sizeof *o->data);
    }
    return o;
}

double density(double t)
{
    return -log10(t > 1e-5 ? t : 1e-5);
}

void raw_density_at(const cz_raw *r, uint32_t x0, uint32_t y0, uint32_t w, uint32_t h, double *out)
{
    double sum[3] = { 0, 0, 0 }, n = 0;
    uint32_t x, y, ye = y0 + h < r->height ? y0 + h : r->height, xe = x0 + w < r->width ? x0 + w : r->width;
    int k;

    for (y = y0; y < ye; y++) {
        for (x = x0; x < xe; x++) {
            const uint16_t *px = r->data + ((size_t)y * r->width + x) * (size_t)r->channels;
            for (k = 0; k < r->channels; k++)
                sum[k] += (double)px[k];
            n += 1.0;
        }
    }
    for (k = 0; k < r->channels; k++)
        out[k] = density((sum[k] / (n > 1.0 ? n : 1.0)) / 65535.0);
}

void processed_free(cz_processed *p)
{
    if (p != NULL) {
        free(p->data);
        free(p);
    }
}

/* ------------------------------------------------------------------ ICC */

static const int32_t ICC_WHITE[3] = { 63190, 65536, 54060 };
static const int32_t ICC_RED[3] = { 36890, 15502, 329 };
static const int32_t ICC_GREEN[3] = { 18668, 48455, 3555 };
static const int32_t ICC_BLUE[3] = { 7619, 1577, 50206 };

static double fixed(int32_t v)
{
    return (double)v / 65536.0;
}

static void be32put(sbuf *b, uint32_t v)
{
    uint8_t t[4];
    t[0] = (uint8_t)(v >> 24);
    t[1] = (uint8_t)(v >> 16);
    t[2] = (uint8_t)(v >> 8);
    t[3] = (uint8_t)v;
    sb_put(b, t, 4);
}

static int32_t s15_val(double v)
{
    double r = round(v * 65536.0);
    if (r >= 2147483647.0)
        return INT32_MAX;
    if (r <= -2147483648.0)
        return INT32_MIN;
    return (int32_t)r;
}

static void xyz_tag(sbuf *t, double x, double y, double z)
{
    sb_put(t, "XYZ \0\0\0\0", 8);
    be32put(t, (uint32_t)s15_val(x));
    be32put(t, (uint32_t)s15_val(y));
    be32put(t, (uint32_t)s15_val(z));
}

static void curve_tag(sbuf *t, double gamma)
{
    uint16_t g = sat_u16(round(gamma * 256.0));
    sb_put(t, "curv\0\0\0\0", 8);
    be32put(t, 1);
    sb_byte(t, (uint8_t)(g >> 8));
    sb_byte(t, (uint8_t)g);
}

static void desc_tag(sbuf *t, const char *s)
{
    static const uint8_t zeros[78] = { 0 };
    size_t n = strlen(s);
    sb_put(t, "desc\0\0\0\0", 8);
    be32put(t, (uint32_t)(n + 1));
    sb_put(t, s, n);
    sb_byte(t, 0);
    sb_put(t, zeros, 8);  /* unicode language and count */
    sb_put(t, zeros, 3);  /* scriptcode code and count */
    sb_put(t, zeros, 67);
}

static void text_tag(sbuf *t, const char *s)
{
    sb_put(t, "text\0\0\0\0", 8);
    sb_put(t, s, strlen(s));
    sb_byte(t, 0);
}

typedef struct {
    const char *sig;
    sbuf data;
} icc_tag;

static void icc_build(const char *cls, const char *space, icc_tag *tags, int count, sbuf *out)
{
    size_t table_len = 4 + 12 * (size_t)count, offset = 128 + table_len;
    sbuf table, body;
    uint8_t h[128];
    int i;

    sb_init(&table);
    sb_init(&body);
    be32put(&table, (uint32_t)count);
    for (i = 0; i < count; i++) {
        while (offset % 4 != 0) {
            sb_byte(&body, 0);
            offset++;
        }
        sb_put(&table, tags[i].sig, 4);
        be32put(&table, (uint32_t)offset);
        be32put(&table, (uint32_t)tags[i].data.len);
        sb_put(&body, tags[i].data.p, tags[i].data.len);
        offset += tags[i].data.len;
    }
    while (offset % 4 != 0) {
        sb_byte(&body, 0);
        offset++;
    }
    memset(h, 0, sizeof h);
    h[0] = (uint8_t)(offset >> 24);
    h[1] = (uint8_t)(offset >> 16);
    h[2] = (uint8_t)(offset >> 8);
    h[3] = (uint8_t)offset;
    h[8] = 0x02;
    h[9] = 0x10;
    memcpy(h + 12, cls, 4);
    memcpy(h + 16, space, 4);
    memcpy(h + 20, "XYZ ", 4);
    {
        static const uint8_t date[12] = { 0x07, 0xEA, 0, 10, 0, 15, 0, 0, 0, 0, 0, 0 }; /* 2026-10-15 */
        memcpy(h + 24, date, 12);
    }
    memcpy(h + 36, "acsp", 4);
    {
        int32_t v[3];
        int k;
        v[0] = s15_val(0.9642);
        v[1] = s15_val(1.0);
        v[2] = s15_val(0.8249);
        for (k = 0; k < 3; k++) {
            uint32_t u = (uint32_t)v[k];
            h[68 + 4 * k] = (uint8_t)(u >> 24);
            h[69 + 4 * k] = (uint8_t)(u >> 16);
            h[70 + 4 * k] = (uint8_t)(u >> 8);
            h[71 + 4 * k] = (uint8_t)u;
        }
    }
    sb_put(out, h, sizeof h);
    sb_put(out, table.p, table.len);
    sb_put(out, body.p, body.len);
    sb_free(&table);
    sb_free(&body);
    for (i = 0; i < count; i++)
        sb_free(&tags[i].data);
}

static void tag_init(icc_tag *t, const char *sig)
{
    t->sig = sig;
    sb_init(&t->data);
}

void icc_output_profile(int channels, sbuf *out)
{
    double w0 = fixed(ICC_WHITE[0]), w1 = fixed(ICC_WHITE[1]), w2 = fixed(ICC_WHITE[2]);
    icc_tag tags[9];
    char name[64];

    if (channels == 3) {
        tag_init(&tags[0], "desc");
        desc_tag(&tags[0].data, "FT-S5000 RGB (SCREEN 5000stdRGB V1.1)");
        tag_init(&tags[1], "cprt");
        text_tag(&tags[1].data, "Made by Cezanne Scan, use freely");
        tag_init(&tags[2], "wtpt");
        xyz_tag(&tags[2].data, w0, w1, w2);
        tag_init(&tags[3], "rXYZ");
        xyz_tag(&tags[3].data, fixed(ICC_RED[0]), fixed(ICC_RED[1]), fixed(ICC_RED[2]));
        tag_init(&tags[4], "gXYZ");
        xyz_tag(&tags[4].data, fixed(ICC_GREEN[0]), fixed(ICC_GREEN[1]), fixed(ICC_GREEN[2]));
        tag_init(&tags[5], "bXYZ");
        xyz_tag(&tags[5].data, fixed(ICC_BLUE[0]), fixed(ICC_BLUE[1]), fixed(ICC_BLUE[2]));
        tag_init(&tags[6], "rTRC");
        curve_tag(&tags[6].data, SCANNER_GAMMA);
        tag_init(&tags[7], "gTRC");
        curve_tag(&tags[7].data, SCANNER_GAMMA);
        tag_init(&tags[8], "bTRC");
        curve_tag(&tags[8].data, SCANNER_GAMMA);
        icc_build("scnr", "RGB ", tags, 9, out);
    } else {
        snprintf(name, sizeof name, "FT-S5000 Grey gamma %.3f", SCANNER_GAMMA);
        tag_init(&tags[0], "desc");
        desc_tag(&tags[0].data, name);
        tag_init(&tags[1], "cprt");
        text_tag(&tags[1].data, "Made by Cezanne Scan, use freely");
        tag_init(&tags[2], "wtpt");
        xyz_tag(&tags[2].data, w0, w1, w2);
        tag_init(&tags[3], "kTRC");
        curve_tag(&tags[3].data, SCANNER_GAMMA);
        icc_build("scnr", "GRAY", tags, 4, out);
    }
}

void icc_scanner_luminance(double w[3])
{
    w[0] = fixed(ICC_RED[1]);
    w[1] = fixed(ICC_GREEN[1]);
    w[2] = fixed(ICC_BLUE[1]);
}

static void invert3(const double m[3][3], double o[3][3])
{
#define C(r, k) (m[((r) + 1) % 3][((k) + 1) % 3] * m[((r) + 2) % 3][((k) + 2) % 3] - \
                 m[((r) + 1) % 3][((k) + 2) % 3] * m[((r) + 2) % 3][((k) + 1) % 3])
    double det = m[0][0] * C(0, 0) + m[0][1] * C(0, 1) + m[0][2] * C(0, 2);
    int i, j;
    for (i = 0; i < 3; i++)
        for (j = 0; j < 3; j++)
            o[i][j] = C(j, i) / det;
#undef C
}

void icc_scanner_to_srgb(double m[3][3])
{
    static const double SRGB_D50[3][3] = { { 0.4361, 0.2225, 0.0139 }, { 0.3851, 0.7169, 0.0971 },
                                           { 0.1431, 0.0606, 0.7141 } };
    double prim[3][3], src[3][3], cols[3][3], dst[3][3];
    int i, j, k;

    for (k = 0; k < 3; k++) {
        prim[0][k] = fixed(ICC_RED[k]);
        prim[1][k] = fixed(ICC_GREEN[k]);
        prim[2][k] = fixed(ICC_BLUE[k]);
    }
    /* cols(): transpose */
    for (i = 0; i < 3; i++)
        for (j = 0; j < 3; j++) {
            src[i][j] = prim[j][i];
            cols[i][j] = SRGB_D50[j][i];
        }
    invert3(cols, dst);
    for (i = 0; i < 3; i++)
        for (j = 0; j < 3; j++) {
            double s = -0.0;
            for (k = 0; k < 3; k++)
                s += dst[i][k] * src[k][j];
            m[i][j] = s;
        }
}

/* ------------------------------------------------------------------ display */

uint8_t *processed_to_rgba8(const cz_processed *p)
{
    static double lin[4096];
    static uint8_t enc[4096];
    static int ready = 0;
    static double m[3][3];
    size_t i, n = (size_t)p->width * p->height;
    uint8_t *out = xmalloc(n * 4);

    if (!ready) {
        int k;
        for (k = 0; k < 4096; k++) {
            double v = (double)k / 4095.0;
            double s = v <= 0.0031308 ? 12.92 * v : 1.055 * pow(v, 1.0 / 2.4) - 0.055;
            lin[k] = pow((double)k / 4095.0, SCANNER_GAMMA);
            enc[k] = sat_u8(clampd(round(s * 255.0), 0.0, 255.0));
        }
        icc_scanner_to_srgb(m);
        ready = 1; /* written once by one thread before any concurrent use, see server.c */
    }
#define TO8(v) enc[(size_t)round(clampd((v), 0.0, 1.0) * 4095.0)]
    for (i = 0; i < n; i++) {
        const uint16_t *px = p->data + i * (size_t)p->channels;
        uint8_t *o = out + i * 4;
        if (p->channels == 3) {
            double c0 = lin[px[0] >> 4], c1 = lin[px[1] >> 4], c2 = lin[px[2] >> 4];
            double s0 = m[0][0] * c0 + m[0][1] * c1 + m[0][2] * c2;
            double s1 = m[1][0] * c0 + m[1][1] * c1 + m[1][2] * c2;
            double s2 = m[2][0] * c0 + m[2][1] * c1 + m[2][2] * c2;
            o[0] = TO8(s0);
            o[1] = TO8(s1);
            o[2] = TO8(s2);
        } else {
            uint8_t g = TO8(lin[px[0] >> 4]);
            o[0] = o[1] = o[2] = g;
        }
        o[3] = 255;
    }
#undef TO8
    return out;
}

/* ------------------------------------------------------------------ films and adjustments */

static const char *const FILM_IDS[] = { "Positive", "ColorNegative", "BwNegative", "Reflective" };
static const char *const FILM_LABELS[] = { "Positive (slide)", "Colour negative", "Black and white negative",
                                           "Print (reflective)" };

const char *film_id(int f) { return FILM_IDS[f]; }
const char *film_label(int f) { return FILM_LABELS[f]; }
int film_is_negative(int f) { return f == FILM_COLOR_NEGATIVE || f == FILM_BW_NEGATIVE; }

/* negatives are scanned as positive film and inverted on the computer, as Cezanne owners do */
int film_original(int f)
{
    return f == FILM_REFLECTIVE ? ORI_PHOTOGRAPH : ORI_POSITIVE_FILM;
}

int film_from_id(const char *s)
{
    int i;
    for (i = 0; i < 4; i++)
        if (strcmp(s, FILM_IDS[i]) == 0)
            return i;
    return -1;
}

void adjust_linear_curve(cz_adjust *a)
{
    a->curve_points = 2;
    a->curve[0][0] = 0.0f;
    a->curve[0][1] = 0.0f;
    a->curve[1][0] = 1.0f;
    a->curve[1][1] = 1.0f;
}

void adjust_s_curve(cz_adjust *a, float strength)
{
    float k = 0.06f * strength;
    a->curve_points = 4;
    a->curve[0][0] = 0.0f;
    a->curve[0][1] = 0.0f;
    a->curve[1][0] = 0.25f;
    a->curve[1][1] = 0.25f - k;
    a->curve[2][0] = 0.75f;
    a->curve[2][1] = 0.75f + k;
    a->curve[3][0] = 1.0f;
    a->curve[3][1] = 1.0f;
}

void adjust_for_film(cz_adjust *a, int film)
{
    float hd, sd;
    int c;
    memset(a, 0, sizeof *a);
    if (film == FILM_POSITIVE || film == FILM_REFLECTIVE) {
        hd = 0.15f;
        sd = 2.8f;
    } else {
        /* flipped values invert the image (Corran's method) */
        hd = 2.2f;
        sd = 0.15f;
    }
    a->film = film;
    for (c = 0; c < 3; c++) {
        a->hd[c] = hd;
        a->sd[c] = sd;
    }
    a->gamma = film_is_negative(film) ? 1.6f : 1.0f;
    adjust_linear_curve(a);
    a->output_min = 0.0f;
    a->output_max = 255.0f;
    a->sharpen_enabled = 0;
    a->sharpen_amount = 0.6f;
    a->sharpen_radius = 1.2f;
    a->gray = film == FILM_BW_NEGATIVE ? 1 : GRAY_KEEP;
    a->rotate = 0;
    a->mirror = 0;
}

void adjust_auto_levels(cz_adjust *a, const cz_raw *img, double clip)
{
    int c = img->channels, ch;
    size_t i, npx = (size_t)img->width * img->height;
    uint32_t *hist = xmalloc(4096 * sizeof *hist);

    for (ch = 0; ch < (c < 3 ? c : 3); ch++) {
        uint64_t total = 0;
        double thin, dense;
        int q;
        memset(hist, 0, 4096 * sizeof *hist);
        for (i = 0; i < npx; i++)
            hist[img->data[i * (size_t)c + (size_t)ch] >> 4]++;
        for (i = 0; i < 4096; i++)
            total += hist[i];
        for (q = 0; q < 2; q++) {
            /* q 0: 1 - clip, the brightest pixels (lowest density). q 1: clip, the darkest */
            double frac = q == 0 ? 1.0 - clip : clip;
            double tf = (double)total * frac;
            uint64_t target = tf <= 0 ? 0 : (tf >= 1.8446744073709552e19 ? UINT64_MAX : (uint64_t)tf);
            uint64_t acc = 0;
            double d = density(1.0);
            for (i = 0; i < 4096; i++) {
                acc += hist[i];
                if (acc >= target) {
                    d = density(((double)i + 0.5) * 16.0 / 65535.0);
                    break;
                }
            }
            if (q == 0)
                thin = d;
            else
                dense = d;
        }
        if (film_is_negative(a->film)) {
            a->hd[ch] = (float)dense;
            a->sd[ch] = (float)thin;
        } else {
            a->hd[ch] = (float)thin;
            a->sd[ch] = (float)dense;
        }
    }
    free(hist);
    if (c == 1) {
        a->hd[1] = a->hd[2] = a->hd[0];
        a->sd[1] = a->sd[2] = a->sd[0];
    }
}

/* ------------------------------------------------------------------ curve */

typedef struct {
    double x, y;
} pt;

static int pt_cmp(const void *a, const void *b)
{
    double x = ((const pt *)a)->x, y = ((const pt *)b)->x;
    return x < y ? -1 : x > y ? 1 : 0;
}

static int curve_is_linear(const cz_adjust *a)
{
    int i;
    for (i = 0; i < a->curve_points; i++)
        if (fabsf(a->curve[i][0] - a->curve[i][1]) >= 1e-6f)
            return 0;
    return 1;
}

/* Curve::sorted: clamped, sorted by x (stable, like sort_by), near duplicates dropped */
static int curve_sorted(const cz_adjust *a, pt *p)
{
    int n = 0, i, j;
    pt tmp[CZ_MAX_CURVE_POINTS];

    for (i = 0; i < a->curve_points; i++) {
        tmp[i].x = (double)clampf(a->curve[i][0], 0.0f, 1.0f);
        tmp[i].y = (double)clampf(a->curve[i][1], 0.0f, 1.0f);
    }
    /* insertion sort, stable like Rust's sort_by */
    for (i = 1; i < a->curve_points; i++) {
        pt v = tmp[i];
        j = i - 1;
        while (j >= 0 && pt_cmp(&tmp[j], &v) > 0) {
            tmp[j + 1] = tmp[j];
            j--;
        }
        tmp[j + 1] = v;
    }
    for (i = 0; i < a->curve_points; i++) {
        if (n > 0 && fabs(tmp[i].x - p[n - 1].x) < 1e-9)
            continue;
        p[n++] = tmp[i];
    }
    if (n < 2) {
        p[0].x = 0.0;
        p[0].y = 0.0;
        p[1].x = 1.0;
        p[1].y = 1.0;
        n = 2;
    }
    return n;
}

/* Fritsch and Carlson monotone cubic, evaluated in place on xs */
static void curve_eval_many(const cz_adjust *a, double *xs, size_t count)
{
    pt p[CZ_MAX_CURVE_POINTS];
    double d[CZ_MAX_CURVE_POINTS], m[CZ_MAX_CURVE_POINTS];
    int n = curve_sorted(a, p), i;
    size_t k;

    for (i = 0; i < n - 1; i++)
        d[i] = (p[i + 1].y - p[i].y) / (p[i + 1].x - p[i].x);
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for (i = 1; i < n - 1; i++)
        m[i] = d[i - 1] * d[i] <= 0.0 ? 0.0 : (d[i - 1] + d[i]) / 2.0;
    for (i = 0; i < n - 1; i++) {
        if (d[i] == 0.0) {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            double aa = m[i] / d[i], bb = m[i + 1] / d[i], s = aa * aa + bb * bb;
            if (s > 9.0) {
                double t = 3.0 / sqrt(s);
                m[i] = t * aa * d[i];
                m[i + 1] = t * bb * d[i];
            }
        }
    }
    for (k = 0; k < count; k++) {
        double x = xs[k], h, t, t2, t3, y;
        if (x <= p[0].x) {
            xs[k] = p[0].y;
            continue;
        }
        if (x >= p[n - 1].x) {
            xs[k] = p[n - 1].y;
            continue;
        }
        for (i = 0; i < n - 1; i++)
            if (x >= p[i].x && x <= p[i + 1].x)
                break;
        if (i >= n - 1)
            i = n - 2;
        h = p[i + 1].x - p[i].x;
        t = (x - p[i].x) / h;
        t2 = t * t;
        t3 = t * t * t;
        y = (2.0 * t3 - 3.0 * t2 + 1.0) * p[i].y + (t3 - 2.0 * t2 + t) * h * m[i] + (-2.0 * t3 + 3.0 * t2) * p[i + 1].y +
            (t3 - t2) * h * m[i + 1];
        xs[k] = clampd(y, 0.0, 1.0);
    }
}

/* ------------------------------------------------------------------ the pipeline */

/* Adjustments::luts: 4096 entries per channel, indexed by input >> 4 */
static void build_luts(const cz_adjust *a, int channels, uint16_t (*luts)[4096])
{
    double lo = (double)clampf(a->output_min, 0.0f, 255.0f) / 255.0;
    double hi = (double)clampf(a->output_max, 0.0f, 255.0f) / 255.0;
    double g = (double)a->gamma > 0.05 ? (double)a->gamma : 0.05;
    double *pos = xmalloc(4096 * sizeof *pos);
    int linear = curve_is_linear(a), ch, i;

    for (ch = 0; ch < channels; ch++) {
        double hd = (double)a->hd[ch < 2 ? ch : 2];
        double sd = (double)a->sd[ch < 2 ? ch : 2];
        double span = fabs(hd - sd) < 1e-4 ? 1e-4 : hd - sd;
        for (i = 0; i < 4096; i++) {
            double t = ((double)i + 0.5) * 16.0 / 65535.0;
            double n = clampd((density(t) - sd) / span, 0.0, 1.0);
            pos[i] = pow(n, 1.0 / g);
        }
        if (!linear)
            curve_eval_many(a, pos, 4096);
        for (i = 0; i < 4096; i++)
            luts[ch][i] = sat_u16(clampd(round((lo + pos[i] * (hi - lo)) * 65535.0), 0.0, 65535.0));
    }
    free(pos);
}

static void to_gray(cz_processed *img, int mode)
{
    size_t i, n = (size_t)img->width * img->height;
    uint16_t *out = xmalloc(n * sizeof *out);
    double w[3];
    int k = mode >= 0 ? (mode < 2 ? mode : 2) : 0;

    icc_scanner_luminance(w);
    for (i = 0; i < n; i++) {
        const uint16_t *p = img->data + i * 3;
        double v = mode == GRAY_LUMINANCE ? w[0] * (double)p[0] + w[1] * (double)p[1] + w[2] * (double)p[2]
                                          : (double)p[k];
        out[i] = sat_u16(clampd(round(v), 0.0, 65535.0));
    }
    free(img->data);
    img->data = out;
    img->channels = 1;
}

static void mirror(cz_processed *img)
{
    size_t w = img->width, c = (size_t)img->channels, y, x, k;
    for (y = 0; y < img->height; y++) {
        uint16_t *row = img->data + y * w * c;
        for (x = 0; x < w / 2; x++)
            for (k = 0; k < c; k++) {
                uint16_t t = row[x * c + k];
                row[x * c + k] = row[(w - 1 - x) * c + k];
                row[(w - 1 - x) * c + k] = t;
            }
    }
}

static void rotate_cw(cz_processed *img)
{
    size_t w = img->width, h = img->height, c = (size_t)img->channels, x, y;
    uint16_t *out = xmalloc(w * h * c * sizeof *out);
    uint32_t tw;
    double td;
    for (y = 0; y < h; y++)
        for (x = 0; x < w; x++) {
            size_t nx = h - 1 - y, ny = x;
            memcpy(out + (ny * h + nx) * c, img->data + (y * w + x) * c, c * sizeof *out);
        }
    free(img->data);
    img->data = out;
    tw = img->width;
    img->width = img->height;
    img->height = tw;
    td = img->x_dpi;
    img->x_dpi = img->y_dpi;
    img->y_dpi = td;
}

static size_t min_sz(size_t a, size_t b) { return a < b ? a : b; }

static void box_h(const float *src, float *dst, size_t w, size_t h, size_t r)
{
    float norm = 1.0f / (float)(2 * r + 1);
    size_t y, x, i;
    for (y = 0; y < h; y++) {
        const float *row = src + y * w;
        float acc = 0.0f;
        for (i = 0; i <= r; i++)
            acc += row[min_sz(i, w - 1)];
        acc = acc + (float)r * row[0];
        for (x = 0; x < w; x++) {
            float add, sub;
            dst[y * w + x] = acc * norm;
            add = row[min_sz(x + r + 1, w - 1)];
            sub = row[x >= r ? x - r : 0];
            acc += add - sub;
        }
    }
}

static void box_v(const float *src, float *dst, size_t w, size_t h, size_t r)
{
    float norm = 1.0f / (float)(2 * r + 1);
    size_t y, x, i;
#define AT(yy) src[min_sz((yy), h - 1) * w + x]
    for (x = 0; x < w; x++) {
        float acc = 0.0f;
        for (i = 0; i <= r; i++)
            acc += AT(i);
        acc = acc + (float)r * AT(0);
        for (y = 0; y < h; y++) {
            dst[y * w + x] = acc * norm;
            acc += AT(y + r + 1) - AT(y >= r ? y - r : 0);
        }
    }
#undef AT
}

/* unsharp mask with a three pass box blur approximating a Gaussian */
static void unsharp(cz_processed *img, float radius, float amount)
{
    size_t w = img->width, h = img->height, c = (size_t)img->channels, n = w * h, i, ch;
    float rf = roundf(radius * 0.8f);
    size_t r = rf < 1.0f ? 1 : (size_t)rf;
    float *src = xmalloc(n * sizeof *src), *blur = xmalloc(n * sizeof *blur), *tmp = xcalloc(n, sizeof *tmp);
    int pass;

    for (ch = 0; ch < c; ch++) {
        for (i = 0; i < n; i++)
            src[i] = (float)img->data[i * c + ch];
        memcpy(blur, src, n * sizeof *src);
        for (pass = 0; pass < 3; pass++) {
            box_h(blur, tmp, w, h, r);
            box_v(tmp, blur, w, h, r);
        }
        for (i = 0; i < n; i++) {
            float s = src[i], b = blur[i];
            float v = s + amount * (s - b);
            float rv = roundf(v);
            img->data[i * c + ch] = sat_u16((double)clampf(rv, 0.0f, 65535.0f));
        }
    }
    free(src);
    free(blur);
    free(tmp);
}

cz_processed *adjust_apply(const cz_adjust *a, const cz_raw *img)
{
    int c = img->channels;
    size_t i, n = (size_t)img->width * img->height * (size_t)c;
    uint16_t (*luts)[4096] = xmalloc((size_t)c * sizeof *luts);
    cz_processed *out = xcalloc(1, sizeof *out);
    unsigned k;

    build_luts(a, c, luts);
    out->width = img->width;
    out->height = img->height;
    out->channels = c;
    out->x_dpi = img->x_dpi;
    out->y_dpi = img->y_dpi;
    out->data = xmalloc(n * sizeof *out->data);
    for (i = 0; i < n; i++)
        out->data[i] = luts[i % (size_t)c][img->data[i] >> 4];
    free(luts);

    if (c == 3 && a->gray != GRAY_KEEP)
        to_gray(out, a->gray);
    if (a->sharpen_enabled && a->sharpen_amount > 0.0f)
        unsharp(out, a->sharpen_radius > 0.3f ? a->sharpen_radius : 0.3f, a->sharpen_amount);
    if (a->mirror)
        mirror(out);
    for (k = 0; k < (unsigned)(a->rotate % 4); k++)
        rotate_cw(out);
    return out;
}

/* ------------------------------------------------------------------ adjustments as JSON */

static void f32_list(sbuf *b, const float *v, int n)
{
    int i;
    sb_byte(b, '[');
    for (i = 0; i < n; i++) {
        if (i)
            sb_byte(b, ',');
        js_f32(b, v[i]);
    }
    sb_byte(b, ']');
}

void adjust_to_json(sbuf *b, const cz_adjust *a)
{
    int i;
    sb_puts(b, "{\"film\":");
    js_str(b, film_id(a->film));
    sb_puts(b, ",\"hd\":");
    f32_list(b, a->hd, 3);
    sb_puts(b, ",\"sd\":");
    f32_list(b, a->sd, 3);
    sb_puts(b, ",\"gamma\":");
    js_f32(b, a->gamma);
    sb_puts(b, ",\"curve\":{\"points\":[");
    for (i = 0; i < a->curve_points; i++) {
        if (i)
            sb_byte(b, ',');
        f32_list(b, a->curve[i], 2);
    }
    sb_puts(b, "]},\"output_min\":");
    js_f32(b, a->output_min);
    sb_puts(b, ",\"output_max\":");
    js_f32(b, a->output_max);
    sb_printf(b, ",\"sharpen\":{\"enabled\":%s,\"amount\":", a->sharpen_enabled ? "true" : "false");
    js_f32(b, a->sharpen_amount);
    sb_puts(b, ",\"radius\":");
    js_f32(b, a->sharpen_radius);
    sb_puts(b, "},\"gray\":");
    if (a->gray == GRAY_KEEP)
        sb_puts(b, "\"Keep\"");
    else if (a->gray == GRAY_LUMINANCE)
        sb_puts(b, "\"Luminance\"");
    else
        sb_printf(b, "{\"Channel\":%d}", a->gray);
    sb_printf(b, ",\"rotate\":%u,\"mirror\":%s}", a->rotate, a->mirror ? "true" : "false");
}

/* serde reads an f32 by reading an f64 first and narrowing it */
static int get_f32(const jv *o, const char *key, float *out, char *err, size_t errlen)
{
    jv *v = jget(o, key);
    if (v == NULL || v->type != J_NUM) {
        snprintf(err, errlen, "%s: expected a number", key);
        return -1;
    }
    *out = (float)v->n;
    return 0;
}

static int get_f32_array(const jv *o, const char *key, float *out, int n, char *err, size_t errlen)
{
    jv *v = jget(o, key);
    int i;
    if (v == NULL || v->type != J_ARR || (int)v->count != n) {
        snprintf(err, errlen, "%s: expected %d numbers", key, n);
        return -1;
    }
    for (i = 0; i < n; i++) {
        if (v->items[i]->type != J_NUM) {
            snprintf(err, errlen, "%s: expected %d numbers", key, n);
            return -1;
        }
        out[i] = (float)v->items[i]->n;
    }
    return 0;
}

static int get_bool(const jv *o, const char *key, int *out, char *err, size_t errlen)
{
    jv *v = jget(o, key);
    if (v == NULL || v->type != J_BOOL) {
        snprintf(err, errlen, "%s: expected true or false", key);
        return -1;
    }
    *out = v->b;
    return 0;
}

static int whole_number(const jv *v, double lo, double hi, double *out)
{
    if (v == NULL || v->type != J_NUM || v->n != floor(v->n) || v->n < lo || v->n > hi)
        return -1;
    *out = v->n;
    return 0;
}

int adjust_from_json(const jv *o, cz_adjust *a, char *err, size_t errlen)
{
    jv *v, *pts;
    double d;
    size_t i;

    memset(a, 0, sizeof *a);
    if (o == NULL || o->type != J_OBJ) {
        snprintf(err, errlen, "adjustments: expected an object");
        return -1;
    }
    v = jget(o, "film");
    if (v == NULL || v->type != J_STR || (a->film = film_from_id(v->s)) < 0) {
        snprintf(err, errlen, "film: unknown film type");
        return -1;
    }
    if (get_f32_array(o, "hd", a->hd, 3, err, errlen) < 0 || get_f32_array(o, "sd", a->sd, 3, err, errlen) < 0 ||
        get_f32(o, "gamma", &a->gamma, err, errlen) < 0 || get_f32(o, "output_min", &a->output_min, err, errlen) < 0 ||
        get_f32(o, "output_max", &a->output_max, err, errlen) < 0 || get_bool(o, "mirror", &a->mirror, err, errlen) < 0)
        return -1;
    v = jget(o, "curve");
    pts = jget(v, "points");
    if (pts == NULL || pts->type != J_ARR || pts->count > CZ_MAX_CURVE_POINTS) {
        snprintf(err, errlen, "curve: expected up to %d points", CZ_MAX_CURVE_POINTS);
        return -1;
    }
    a->curve_points = (int)pts->count;
    for (i = 0; i < pts->count; i++) {
        jv *p = pts->items[i];
        if (p->type != J_ARR || p->count != 2 || p->items[0]->type != J_NUM || p->items[1]->type != J_NUM) {
            snprintf(err, errlen, "curve: each point is [x, y]");
            return -1;
        }
        a->curve[i][0] = (float)p->items[0]->n;
        a->curve[i][1] = (float)p->items[1]->n;
    }
    v = jget(o, "sharpen");
    if (v == NULL || v->type != J_OBJ || get_bool(v, "enabled", &a->sharpen_enabled, err, errlen) < 0 ||
        get_f32(v, "amount", &a->sharpen_amount, err, errlen) < 0 ||
        get_f32(v, "radius", &a->sharpen_radius, err, errlen) < 0)
        return -1;
    v = jget(o, "gray");
    if (v != NULL && v->type == J_STR && strcmp(v->s, "Keep") == 0) {
        a->gray = GRAY_KEEP;
    } else if (v != NULL && v->type == J_STR && strcmp(v->s, "Luminance") == 0) {
        a->gray = GRAY_LUMINANCE;
    } else if (v != NULL && v->type == J_OBJ && whole_number(jget(v, "Channel"), 0, 255, &d) == 0) {
        a->gray = (int)d; /* pipeline uses min(k, 2) */
    } else {
        snprintf(err, errlen, "gray: expected Keep, Luminance or {\"Channel\": n}");
        return -1;
    }
    if (whole_number(jget(o, "rotate"), 0, 255, &d) < 0) {
        snprintf(err, errlen, "rotate: expected 0 to 255");
        return -1;
    }
    a->rotate = (uint8_t)d;
    return 0;
}

/* ------------------------------------------------------------------ TIFF */

static void le16(uint8_t *b, uint16_t v)
{
    b[0] = (uint8_t)v;
    b[1] = (uint8_t)(v >> 8);
}

static void le32(uint8_t *b, uint32_t v)
{
    b[0] = (uint8_t)v;
    b[1] = (uint8_t)(v >> 8);
    b[2] = (uint8_t)(v >> 16);
    b[3] = (uint8_t)(v >> 24);
}

static uint32_t extra_put(sbuf *extra, uint32_t extra_start, const void *bytes, size_t n)
{
    uint32_t at;
    if (extra->len % 2 == 1)
        sb_byte(extra, 0);
    at = extra_start + (uint32_t)extra->len;
    sb_put(extra, bytes, n);
    return at;
}

static void rational(uint8_t out[8], double v)
{
    double r = round(v * 100.0);
    le32(out, sat_u32(r > 1.0 ? r : 1.0));
    le32(out + 4, 100);
}

typedef struct {
    uint16_t tag, type;
    uint32_t count, value;
} ifd_entry;

static int entry_cmp(const void *a, const void *b)
{
    return (int)((const ifd_entry *)a)->tag - (int)((const ifd_entry *)b)->tag;
}

int tiff_write(const char *path, const cz_processed *img, int sixteen, const uint8_t *icc, size_t icc_len, cz_err *e)
{
    uint32_t spp = (uint32_t)img->channels, bps = sixteen ? 16 : 8, rows_per_strip, strips, i;
    uint64_t row_bytes, image_bytes, rps;
    uint32_t extra_start, offsets_at, counts_at, bps_at, xres_at, yres_at, soft_at, icc_at = 0, ifd_at;
    static const char software[] = "Cezanne Scan"; /* written with its NUL, 13 bytes */
    sbuf extra, ifd;
    uint8_t tmp[8], *row;
    ifd_entry entries[16];
    int ne = 0, k;
    FILE *f;
    size_t per_row, y;

    if (spp != 1 && spp != 3)
        return cz_fail(e, E_INVALID, "TIFF output needs 1 or 3 channels");
    row_bytes = (uint64_t)img->width * spp * (bps / 8);
    rps = (1ull << 20) / (row_bytes > 1 ? row_bytes : 1);
    if (rps < 1)
        rps = 1;
    if (rps > img->height)
        rps = img->height;
    rows_per_strip = (uint32_t)rps;
    strips = (img->height + rows_per_strip - 1) / rows_per_strip;
    image_bytes = row_bytes * img->height;
    if (8 + image_bytes + 4096 + icc_len > 0xFFFFFFFFull)
        return cz_fail(e, E_INVALID, "image too large for a classic TIFF (over 4 GB)");

    f = fopen(path, "wb");
    if (f == NULL)
        return cz_fail(e, E_IO, "%s: %s", path, strerror(errno));
    setvbuf(f, NULL, _IOFBF, 1 << 20);

    sb_init(&extra);
    extra_start = 8 + (uint32_t)image_bytes;
    {
        uint32_t *offs = xmalloc(strips * sizeof *offs), *cnts = xmalloc(strips * sizeof *cnts);
        uint8_t *lebuf = xmalloc((size_t)strips * 4);
        for (i = 0; i < strips; i++) {
            uint32_t rows = rows_per_strip < img->height - i * rows_per_strip ? rows_per_strip
                                                                              : img->height - i * rows_per_strip;
            offs[i] = 8 + (uint32_t)((uint64_t)i * rows_per_strip * row_bytes);
            cnts[i] = (uint32_t)((uint64_t)rows * row_bytes);
        }
        if (strips > 1) {
            for (i = 0; i < strips; i++)
                le32(lebuf + 4 * i, offs[i]);
            offsets_at = extra_put(&extra, extra_start, lebuf, (size_t)strips * 4);
            for (i = 0; i < strips; i++)
                le32(lebuf + 4 * i, cnts[i]);
            counts_at = extra_put(&extra, extra_start, lebuf, (size_t)strips * 4);
        } else {
            offsets_at = offs[0];
            counts_at = cnts[0];
        }
        free(offs);
        free(cnts);
        free(lebuf);
    }
    if (spp > 2) {
        uint8_t b[6];
        for (k = 0; k < 3; k++)
            le16(b + 2 * k, (uint16_t)bps);
        bps_at = extra_put(&extra, extra_start, b, 6);
    } else {
        bps_at = bps;
    }
    rational(tmp, img->x_dpi);
    xres_at = extra_put(&extra, extra_start, tmp, 8);
    rational(tmp, img->y_dpi);
    yres_at = extra_put(&extra, extra_start, tmp, 8);
    soft_at = extra_put(&extra, extra_start, software, sizeof software);
    if (icc != NULL)
        icc_at = extra_put(&extra, extra_start, icc, icc_len);

    /* header, image strips, extra data, directory */
    fwrite("II*\0\0\0\0\0", 1, 8, f);
    per_row = (size_t)img->width * (size_t)img->channels;
    row = xmalloc((size_t)row_bytes ? (size_t)row_bytes : 1);
    for (y = 0; y < img->height; y++) {
        const uint16_t *src = img->data + y * per_row;
        size_t x;
        if (bps == 16) {
            for (x = 0; x < per_row; x++)
                le16(row + 2 * x, src[x]);
        } else {
            for (x = 0; x < per_row; x++)
                row[x] = (uint8_t)(((uint32_t)src[x] * 255 + 32767) / 65535);
        }
        fwrite(row, 1, (size_t)row_bytes, f);
    }
    free(row);
    fwrite(extra.p, 1, extra.len, f);
    if (extra.len % 2 == 1)
        fputc(0, f);
    ifd_at = extra_start + (uint32_t)extra.len + (uint32_t)(extra.len % 2);

#define ADD(t, ty, c, v) (entries[ne].tag = (t), entries[ne].type = (ty), entries[ne].count = (c), \
                          entries[ne].value = (v), ne++)
    ADD(256, 4, 1, img->width);
    ADD(257, 4, 1, img->height);
    ADD(258, 3, spp, bps_at);
    ADD(259, 3, 1, 1);
    ADD(262, 3, 1, spp == 3 ? 2 : 1);
    ADD(273, 4, strips, offsets_at);
    ADD(277, 3, 1, spp);
    ADD(278, 4, 1, rows_per_strip);
    ADD(279, 4, strips, counts_at);
    ADD(282, 5, 1, xres_at);
    ADD(283, 5, 1, yres_at);
    ADD(284, 3, 1, 1);
    ADD(296, 3, 1, 2);
    ADD(305, 2, (uint32_t)sizeof software, soft_at);
    if (icc != NULL)
        ADD(34675, 7, (uint32_t)icc_len, icc_at);
#undef ADD
    qsort(entries, (size_t)ne, sizeof entries[0], entry_cmp);

    sb_init(&ifd);
    le16(tmp, (uint16_t)ne);
    sb_put(&ifd, tmp, 2);
    for (k = 0; k < ne; k++) {
        uint8_t b[12];
        le16(b, entries[k].tag);
        le16(b + 2, entries[k].type);
        le32(b + 4, entries[k].count);
        if (entries[k].type == 3 && entries[k].count == 1) {
            le16(b + 8, (uint16_t)entries[k].value);
            b[10] = b[11] = 0;
        } else {
            le32(b + 8, entries[k].value);
        }
        sb_put(&ifd, b, 12);
    }
    memset(tmp, 0, 4);
    sb_put(&ifd, tmp, 4);
    fwrite(ifd.p, 1, ifd.len, f);
    sb_free(&ifd);
    sb_free(&extra);

    le32(tmp, ifd_at);
    if (fseek(f, 4, SEEK_SET) != 0 || fwrite(tmp, 1, 4, f) != 4) {
        fclose(f);
        return cz_fail(e, E_IO, "%s: write failed", path);
    }
    if (ferror(f)) {
        fclose(f);
        return cz_fail(e, E_IO, "%s: write failed (disk full?)", path);
    }
    if (fclose(f) != 0)
        return cz_fail(e, E_IO, "%s: %s", path, strerror(errno));
    return 0;
}

int tiff_read_info(const char *path, uint32_t *w, uint32_t *h, uint16_t *bps, uint16_t *spp)
{
    FILE *f = fopen(path, "rb");
    uint8_t head[8], nb[2], *entries;
    uint32_t ifd;
    size_t n, i;
    int ok = -1;

    if (f == NULL)
        return -1;
    *w = *h = 0;
    *bps = *spp = 0;
    if (fread(head, 1, 8, f) != 8 || memcmp(head, "II*\0", 4) != 0)
        goto out;
    ifd = (uint32_t)head[4] | ((uint32_t)head[5] << 8) | ((uint32_t)head[6] << 16) | ((uint32_t)head[7] << 24);
    if (fseek(f, (long)ifd, SEEK_SET) != 0 || fread(nb, 1, 2, f) != 2)
        goto out;
    n = (size_t)(nb[0] | (nb[1] << 8));
    entries = xmalloc(n * 12 + 1);
    if (fread(entries, 1, n * 12, f) != n * 12) {
        free(entries);
        goto out;
    }
    for (i = 0; i < n; i++) {
        const uint8_t *e = entries + 12 * i;
        uint16_t tag = (uint16_t)(e[0] | (e[1] << 8)), typ = (uint16_t)(e[2] | (e[3] << 8));
        uint32_t count = (uint32_t)e[4] | ((uint32_t)e[5] << 8) | ((uint32_t)e[6] << 16) | ((uint32_t)e[7] << 24);
        uint32_t v32 = (uint32_t)e[8] | ((uint32_t)e[9] << 8) | ((uint32_t)e[10] << 16) | ((uint32_t)e[11] << 24);
        uint16_t v16 = (uint16_t)(e[8] | (e[9] << 8));
        if (tag == 256)
            *w = v32;
        else if (tag == 257)
            *h = v32;
        else if (tag == 258) {
            if (count == 1) {
                *bps = v16;
            } else {
                long back = ftell(f);
                if (fseek(f, (long)v32, SEEK_SET) != 0 || fread(nb, 1, 2, f) != 2) {
                    free(entries);
                    goto out;
                }
                *bps = (uint16_t)(nb[0] | (nb[1] << 8));
                fseek(f, back, SEEK_SET);
            }
        } else if (tag == 277)
            *spp = typ == 3 ? v16 : (uint16_t)v32;
    }
    free(entries);
    ok = 0;
out:
    fclose(f);
    return ok;
}

/* ------------------------------------------------------------------ PNG (stored deflate, for previews) */

static uint32_t crc_table[256];
static int crc_ready = 0;

static uint32_t crc32_update(uint32_t crc, const uint8_t *data, size_t n)
{
    size_t i;
    if (!crc_ready) {
        uint32_t c;
        int k, j;
        for (k = 0; k < 256; k++) {
            c = (uint32_t)k;
            for (j = 0; j < 8; j++)
                c = (c & 1) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
            crc_table[k] = c;
        }
        crc_ready = 1;
    }
    for (i = 0; i < n; i++)
        crc = crc_table[(crc ^ data[i]) & 0xFF] ^ (crc >> 8);
    return crc;
}

static void png_chunk(sbuf *out, const char *kind, const uint8_t *data, size_t n)
{
    uint32_t crc = 0xFFFFFFFFu;
    be32put(out, (uint32_t)n);
    sb_put(out, kind, 4);
    sb_put(out, data, n);
    crc = crc32_update(crc, (const uint8_t *)kind, 4);
    crc = crc32_update(crc, data, n);
    be32put(out, crc ^ 0xFFFFFFFFu);
}

void png_encode_rgba(uint32_t w, uint32_t h, const uint8_t *rgba, sbuf *out)
{
    size_t row = (size_t)w * 4, raw_len = (row + 1) * h, i, y, blocks;
    uint8_t *raw = xmalloc(raw_len ? raw_len : 1);
    sbuf z;
    uint32_t a = 1, b = 0;
    uint8_t ihdr[13];

    for (y = 0; y < h; y++) {
        raw[y * (row + 1)] = 0;
        memcpy(raw + y * (row + 1) + 1, rgba + y * row, row);
    }
    sb_init(&z);
    sb_byte(&z, 0x78);
    sb_byte(&z, 0x01);
    blocks = (raw_len + 65534) / 65535;
    for (i = 0; i < blocks; i++) {
        size_t start = i * 65535, len = raw_len - start < 65535 ? raw_len - start : 65535;
        uint8_t hd[5];
        hd[0] = (uint8_t)(i + 1 == blocks);
        hd[1] = (uint8_t)len;
        hd[2] = (uint8_t)(len >> 8);
        hd[3] = (uint8_t)~len;
        hd[4] = (uint8_t)(~len >> 8);
        sb_put(&z, hd, 5);
        sb_put(&z, raw + start, len);
    }
    /* adler32 in chunks of 5552 as png.rs */
    for (i = 0; i < raw_len; i += 5552) {
        size_t k, end = raw_len - i < 5552 ? raw_len : i + 5552;
        for (k = i; k < end; k++) {
            a += raw[k];
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    be32put(&z, (b << 16) | a);
    free(raw);

    sb_put(out, "\x89PNG\r\n\x1a\n", 8);
    ihdr[0] = (uint8_t)(w >> 24);
    ihdr[1] = (uint8_t)(w >> 16);
    ihdr[2] = (uint8_t)(w >> 8);
    ihdr[3] = (uint8_t)w;
    ihdr[4] = (uint8_t)(h >> 24);
    ihdr[5] = (uint8_t)(h >> 16);
    ihdr[6] = (uint8_t)(h >> 8);
    ihdr[7] = (uint8_t)h;
    ihdr[8] = 8;
    ihdr[9] = 6;
    ihdr[10] = ihdr[11] = ihdr[12] = 0;
    png_chunk(out, "IHDR", ihdr, 13);
    png_chunk(out, "IDAT", (const uint8_t *)z.p, z.len);
    png_chunk(out, "IEND", (const uint8_t *)"", 0);
    sb_free(&z);
}
