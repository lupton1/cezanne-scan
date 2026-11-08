/*
 * cezanne-server in C: shared declarations.
 *
 * This is a port of the Rust cezanne-server (crates/cezanne-server) and the parts of
 * cezanne-core it uses, for Macs that cannot run Rust programs (Mac OS X 10.6 to 10.11).
 * Every function here mirrors a Rust function of the same name and does its arithmetic in
 * the same order and precision, so a scan saved by this server is byte for byte the file
 * the Rust server saves. See README.md.
 */
#ifndef CZ_H
#define CZ_H

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#define CZ_VERSION "1.0"
#define CZ_DEFAULT_PORT 7820
#define CZ_BRIDGE_PORT 7810

/* ------------------------------------------------------------------ errors */

enum cz_err_kind {
    E_NONE = 0,
    E_IO,
    E_SCSI,
    E_PROTOCOL,
    E_INVALID,
    E_BLOCKED,
    E_TIMEOUT,
    E_CANCELLED,
    E_NOT_FOUND,
    E_UNSUPPORTED
};

/* SCSI sense keys, the values of the Rust SenseKey enum */
enum { SK_NO_SENSE = 0, SK_RECOVERED = 1, SK_NOT_READY = 2, SK_MEDIUM = 3, SK_HARDWARE = 4,
       SK_ILLEGAL = 5, SK_UNIT_ATTENTION = 6, SK_DATA_PROTECT = 7, SK_ABORTED = 0xB };

typedef struct {
    int kind;
    /* E_SCSI only */
    uint8_t opcode, status;
    int has_sense;
    uint8_t sense_key, asc, ascq;
    /* the message as the Rust Error prints it */
    char msg[600];
} cz_err;

/* Fill e and return -1. The text is formatted like the Rust Error Display. */
int cz_fail(cz_err *e, int kind, const char *fmt, ...);

/* ------------------------------------------------------------------ small utilities */

typedef struct {
    char *p;
    size_t len, cap;
} sbuf;

void sb_init(sbuf *b);
void sb_free(sbuf *b);
void sb_put(sbuf *b, const void *data, size_t n);
void sb_puts(sbuf *b, const char *s);
void sb_printf(sbuf *b, const char *fmt, ...);
void sb_byte(sbuf *b, uint8_t v);
/* NUL terminates without counting the terminator */
const char *sb_str(sbuf *b);

void *xmalloc(size_t n);
void *xcalloc(size_t n, size_t size);
void *xrealloc(void *p, size_t n);
char *xstrdup(const char *s);
char *xstrndup(const char *s, size_t n);

void log_line(int error, const char *fmt, ...);
#define log_info(...) log_line(0, __VA_ARGS__)
#define log_warn(...) log_line(1, __VA_ARGS__)

double now_seconds(void);
void sleep_ms(unsigned ms);
uint64_t unix_time(void);

/* mkdir -p. Returns 0 or -1 with errno set. */
int make_dirs(const char *path);
int file_exists(const char *path);

/* ------------------------------------------------------------------ JSON */

enum { J_NULL, J_BOOL, J_NUM, J_STR, J_ARR, J_OBJ };

typedef struct jv jv;
struct jv {
    int type;
    int b;
    double n;
    char *s;           /* string value */
    size_t count;      /* array items or object members */
    jv **items;
    char **keys;       /* object member names */
};

jv *json_parse(const char *text, size_t len, char *err, size_t errlen);
void json_free(jv *v);
jv *jget(const jv *obj, const char *key);

/* writers */
void js_str(sbuf *b, const char *s);
void js_f64(sbuf *b, double v);   /* shortest form that reads back as the same double */
void js_f32(sbuf *b, float v);    /* shortest form that reads back as the same float */

/* ------------------------------------------------------------------ SCSI transport */

enum { DIR_NONE = 0, DIR_IN = 1, DIR_OUT = 2 };
#define SENSE_LEN 18

typedef struct {
    uint8_t status;
    uint32_t transferred;
    int has_sense;
    uint8_t sense[SENSE_LEN];
} cz_completion;

typedef struct cz_transport cz_transport;
struct cz_transport {
    int (*execute)(cz_transport *t, const uint8_t *cdb, size_t cdb_len, int dir, uint8_t *buf, size_t len,
                   unsigned timeout_ms, cz_completion *c, cz_err *e);
    void (*close)(cz_transport *t);
    char desc[300];
};

/* NULL when the command may be sent, otherwise why it never is (firmware or EEPROM write) */
const char *cz_blocked_reason(const uint8_t *cdb, size_t len);
/* runs a command and turns a status other than GOOD into an E_SCSI error */
int cz_scsi_run(cz_transport *t, const uint8_t *cdb, size_t cdb_len, int dir, uint8_t *buf, size_t len,
                unsigned timeout_ms, size_t *transferred, cz_err *e);
/* "NotReady ASC 0x04 ASCQ 0x00 (scanner is warming up or busy)" */
void cz_sense_text(uint8_t key, uint8_t asc, uint8_t ascq, char *out, size_t size);

cz_transport *cz_tcp_connect(const char *addr, const char *token, cz_err *e);

typedef struct {
    char address[64];
    char vendor[64];
    char product[64];
    char revision[32];
} cz_device;

/* Supported scanners on this computer's SCSI buses. Returns the count or -1. */
int cz_discover(cz_device *out, int max, cz_err *e);
cz_transport *cz_open_local(const char *address, cz_err *e);
/* every device IOKit lists, for --list */
int cz_list_all_devices(FILE *f);

/* ------------------------------------------------------------------ protocol and driver */

enum cz_mode { MODE_GRAY8, MODE_GRAY16, MODE_RGB24, MODE_RGB48 };
enum cz_original { ORI_POSITIVE_FILM, ORI_NEGATIVE_FILM, ORI_PHOTOGRAPH, ORI_PRINTED, ORI_WATERCOLOR };
enum cz_area_unit { UNIT_TENTH_MM, UNIT_INCH1200 };

const char *mode_id(int m);
const char *mode_label(int m);
int mode_channels(int m);
int mode_bytes_per_sample(int m);
int mode_from_id(const char *s);
const char *original_label(int o);
int original_is_reflective(int o);
double unit_per_mm(int u);
const char *unit_id(int u);
int unit_from_id(const char *s);

typedef struct {
    uint32_t max_x, max_y;
    uint16_t max_pixels_per_line;
    uint32_t resolution_min, resolution_max;
} cz_limits;

extern const cz_limits LIMITS_FT_S5000;
extern const cz_limits LIMITS_FT_S3000;
void limits_bed_mm(const cz_limits *l, int unit, double *w, double *h);

typedef struct {
    double x_mm, y_mm, width_mm, height_mm;
} cz_area;

typedef struct {
    uint8_t window_id;
    cz_area area;
    double x_dpi, y_dpi;
    int mode;
    int original;
    int preview;
    int area_unit;
    uint32_t resolution_scale;
} cz_window;

typedef struct {
    uint32_t pixels_per_line, lines;
    int mode;
} cz_geometry;

void window_init(cz_window *w, cz_area area, double dpi, int mode);
cz_geometry window_geometry(const cz_window *w);
size_t geometry_bytes_per_line(const cz_geometry *g);
size_t geometry_total_bytes(const cz_geometry *g);

typedef struct cz_raw cz_raw;

typedef struct {
    cz_transport *t;
    char vendor[16], product[24], revision[8];
    uint8_t vendor_specific[64];
    size_t vendor_specific_len;
    cz_limits limits;
    int reserved;
} cz_scanner;

typedef void (*cz_progress_fn)(void *ctx, float p);

/* takes ownership of t, also on failure */
cz_scanner *scanner_open(cz_transport *t, cz_err *e);
void scanner_close(cz_scanner *s);
void scanner_describe(const cz_scanner *s, char *out, size_t size);
cz_raw *scanner_scan(cz_scanner *s, const cz_window *w, cz_progress_fn progress, void *ctx,
                     volatile int *cancel, cz_err *e);
cz_raw *scanner_preview(cz_scanner *s, const cz_window *w, cz_progress_fn progress, void *ctx,
                        volatile int *cancel, cz_err *e);
/* read only identification report, a new string */
char *scanner_probe(cz_scanner *s, cz_err *e);

/* ------------------------------------------------------------------ images */

struct cz_raw {
    uint32_t width, height;
    int channels;
    double x_dpi, y_dpi;
    int source_bits;
    uint16_t *data;
};

typedef struct {
    uint32_t width, height;
    int channels;
    double x_dpi, y_dpi;
    uint16_t *data;
} cz_processed;

cz_raw *raw_from_scanner(const cz_geometry *g, const uint8_t *bytes, size_t n, double x_dpi, double y_dpi,
                         cz_err *e);
void raw_free(cz_raw *r);
cz_raw *raw_crop(const cz_raw *r, uint32_t x0, uint32_t y0, uint32_t w, uint32_t h);
void raw_density_at(const cz_raw *r, uint32_t x0, uint32_t y0, uint32_t w, uint32_t h, double *out);
void processed_free(cz_processed *p);
/* 8 bit sRGB RGBA, a new buffer of width * height * 4 */
uint8_t *processed_to_rgba8(const cz_processed *p);

double density(double t);

enum cz_film { FILM_POSITIVE, FILM_COLOR_NEGATIVE, FILM_BW_NEGATIVE, FILM_REFLECTIVE };
const char *film_id(int f);
const char *film_label(int f);
int film_is_negative(int f);
int film_original(int f);
int film_from_id(const char *s);

enum { GRAY_KEEP = -2, GRAY_LUMINANCE = -1 }; /* 0, 1, 2 keep one channel */

#define CZ_MAX_CURVE_POINTS 64

typedef struct {
    int film;
    float hd[3], sd[3];
    float gamma;
    int curve_points;
    float curve[CZ_MAX_CURVE_POINTS][2];
    float output_min, output_max;
    int sharpen_enabled;
    float sharpen_amount, sharpen_radius;
    int gray;
    uint8_t rotate;
    int mirror;
} cz_adjust;

void adjust_for_film(cz_adjust *a, int film);
void adjust_auto_levels(cz_adjust *a, const cz_raw *img, double clip);
cz_processed *adjust_apply(const cz_adjust *a, const cz_raw *img);
void adjust_s_curve(cz_adjust *a, float strength);
void adjust_linear_curve(cz_adjust *a);
/* JSON in the shape serde gives the Rust Adjustments */
void adjust_to_json(sbuf *b, const cz_adjust *a);
int adjust_from_json(const jv *v, cz_adjust *a, char *err, size_t errlen);

/* ICC */
#define SCANNER_GAMMA (586.0 / 256.0)
void icc_output_profile(int channels, sbuf *out);
void icc_scanner_luminance(double w[3]);
void icc_scanner_to_srgb(double m[3][3]);

/* TIFF and PNG */
int tiff_write(const char *path, const cz_processed *img, int sixteen, const uint8_t *icc, size_t icc_len,
               cz_err *e);
/* width, height, bits per sample, samples per pixel of a file written by tiff_write */
int tiff_read_info(const char *path, uint32_t *w, uint32_t *h, uint16_t *bps, uint16_t *spp);
void png_encode_rgba(uint32_t w, uint32_t h, const uint8_t *rgba, sbuf *out);

#endif
