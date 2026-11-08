/* A small JSON reader and writer, enough for the web page's requests and answers. */
#include "cz.h"

#include <math.h>
#include <stdlib.h>
#include <string.h>

#define MAX_DEPTH 64

typedef struct {
    const char *p, *end;
    char *err;
    size_t errlen;
    int failed;
} parser;

static void perr(parser *ps, const char *msg)
{
    if (!ps->failed && ps->err != NULL)
        snprintf(ps->err, ps->errlen, "%s", msg);
    ps->failed = 1;
}

static void skip_ws(parser *ps)
{
    while (ps->p < ps->end && (*ps->p == ' ' || *ps->p == '\t' || *ps->p == '\n' || *ps->p == '\r'))
        ps->p++;
}

static jv *new_value(int type)
{
    jv *v = xcalloc(1, sizeof *v);
    v->type = type;
    return v;
}

static void put_utf8(sbuf *b, unsigned cp)
{
    if (cp < 0x80) {
        sb_byte(b, (uint8_t)cp);
    } else if (cp < 0x800) {
        sb_byte(b, (uint8_t)(0xC0 | (cp >> 6)));
        sb_byte(b, (uint8_t)(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
        sb_byte(b, (uint8_t)(0xE0 | (cp >> 12)));
        sb_byte(b, (uint8_t)(0x80 | ((cp >> 6) & 0x3F)));
        sb_byte(b, (uint8_t)(0x80 | (cp & 0x3F)));
    } else {
        sb_byte(b, (uint8_t)(0xF0 | (cp >> 18)));
        sb_byte(b, (uint8_t)(0x80 | ((cp >> 12) & 0x3F)));
        sb_byte(b, (uint8_t)(0x80 | ((cp >> 6) & 0x3F)));
        sb_byte(b, (uint8_t)(0x80 | (cp & 0x3F)));
    }
}

static int hex4(parser *ps, unsigned *out)
{
    unsigned v = 0;
    int i;
    if (ps->end - ps->p < 4)
        return -1;
    for (i = 0; i < 4; i++) {
        char c = *ps->p++;
        v <<= 4;
        if (c >= '0' && c <= '9')
            v |= (unsigned)(c - '0');
        else if (c >= 'a' && c <= 'f')
            v |= (unsigned)(c - 'a' + 10);
        else if (c >= 'A' && c <= 'F')
            v |= (unsigned)(c - 'A' + 10);
        else
            return -1;
    }
    *out = v;
    return 0;
}

static char *parse_string(parser *ps)
{
    sbuf b;
    sb_init(&b);
    ps->p++; /* opening quote */
    while (ps->p < ps->end) {
        unsigned char c = (unsigned char)*ps->p++;
        if (c == '"')
            return (char *)(sb_str(&b), b.p);
        if (c < 0x20) {
            perr(ps, "control character in a string");
            break;
        }
        if (c != '\\') {
            sb_byte(&b, c);
            continue;
        }
        if (ps->p >= ps->end)
            break;
        c = (unsigned char)*ps->p++;
        switch (c) {
        case '"': sb_byte(&b, '"'); break;
        case '\\': sb_byte(&b, '\\'); break;
        case '/': sb_byte(&b, '/'); break;
        case 'b': sb_byte(&b, '\b'); break;
        case 'f': sb_byte(&b, '\f'); break;
        case 'n': sb_byte(&b, '\n'); break;
        case 'r': sb_byte(&b, '\r'); break;
        case 't': sb_byte(&b, '\t'); break;
        case 'u': {
            unsigned cp, lo;
            if (hex4(ps, &cp) < 0) {
                perr(ps, "bad \\u escape");
                sb_free(&b);
                return NULL;
            }
            if (cp >= 0xD800 && cp < 0xDC00 && ps->end - ps->p >= 6 && ps->p[0] == '\\' && ps->p[1] == 'u') {
                ps->p += 2;
                if (hex4(ps, &lo) == 0 && lo >= 0xDC00 && lo < 0xE000)
                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                else
                    cp = 0xFFFD;
            } else if (cp >= 0xD800 && cp < 0xE000) {
                cp = 0xFFFD;
            }
            put_utf8(&b, cp);
            break;
        }
        default:
            perr(ps, "bad escape in a string");
            sb_free(&b);
            return NULL;
        }
    }
    perr(ps, "unterminated string");
    sb_free(&b);
    return NULL;
}

static jv *parse_value(parser *ps, int depth);

static jv *parse_number(parser *ps)
{
    const char *start = ps->p;
    char tmp[64];
    jv *v;
    size_t n;

    if (ps->p < ps->end && *ps->p == '-')
        ps->p++;
    while (ps->p < ps->end && ((*ps->p >= '0' && *ps->p <= '9') || *ps->p == '.' || *ps->p == 'e' ||
                               *ps->p == 'E' || *ps->p == '+' || *ps->p == '-'))
        ps->p++;
    n = (size_t)(ps->p - start);
    if (n == 0 || n >= sizeof tmp) {
        perr(ps, "bad number");
        return NULL;
    }
    memcpy(tmp, start, n);
    tmp[n] = 0;
    v = new_value(J_NUM);
    {
        char *endp;
        v->n = strtod(tmp, &endp);
        if (*endp != 0) {
            perr(ps, "bad number");
            free(v);
            return NULL;
        }
    }
    return v;
}

static jv *parse_array(parser *ps, int depth)
{
    jv *v = new_value(J_ARR);
    size_t cap = 0;
    ps->p++;
    skip_ws(ps);
    if (ps->p < ps->end && *ps->p == ']') {
        ps->p++;
        return v;
    }
    for (;;) {
        jv *item = parse_value(ps, depth + 1);
        if (item == NULL) {
            json_free(v);
            return NULL;
        }
        if (v->count == cap) {
            cap = cap ? cap * 2 : 4;
            v->items = xrealloc(v->items, cap * sizeof *v->items);
        }
        v->items[v->count++] = item;
        skip_ws(ps);
        if (ps->p < ps->end && *ps->p == ',') {
            ps->p++;
            continue;
        }
        if (ps->p < ps->end && *ps->p == ']') {
            ps->p++;
            return v;
        }
        perr(ps, "expected , or ] in an array");
        json_free(v);
        return NULL;
    }
}

static jv *parse_object(parser *ps, int depth)
{
    jv *v = new_value(J_OBJ);
    size_t cap = 0;
    ps->p++;
    skip_ws(ps);
    if (ps->p < ps->end && *ps->p == '}') {
        ps->p++;
        return v;
    }
    for (;;) {
        char *key;
        jv *item;
        skip_ws(ps);
        if (ps->p >= ps->end || *ps->p != '"') {
            perr(ps, "expected a member name");
            json_free(v);
            return NULL;
        }
        key = parse_string(ps);
        if (key == NULL) {
            json_free(v);
            return NULL;
        }
        skip_ws(ps);
        if (ps->p >= ps->end || *ps->p != ':') {
            free(key);
            perr(ps, "expected :");
            json_free(v);
            return NULL;
        }
        ps->p++;
        item = parse_value(ps, depth + 1);
        if (item == NULL) {
            free(key);
            json_free(v);
            return NULL;
        }
        if (v->count == cap) {
            cap = cap ? cap * 2 : 4;
            v->items = xrealloc(v->items, cap * sizeof *v->items);
            v->keys = xrealloc(v->keys, cap * sizeof *v->keys);
        }
        v->keys[v->count] = key;
        v->items[v->count++] = item;
        skip_ws(ps);
        if (ps->p < ps->end && *ps->p == ',') {
            ps->p++;
            continue;
        }
        if (ps->p < ps->end && *ps->p == '}') {
            ps->p++;
            return v;
        }
        perr(ps, "expected , or } in an object");
        json_free(v);
        return NULL;
    }
}

static int literal(parser *ps, const char *word)
{
    size_t n = strlen(word);
    if ((size_t)(ps->end - ps->p) >= n && memcmp(ps->p, word, n) == 0) {
        ps->p += n;
        return 1;
    }
    return 0;
}

static jv *parse_value(parser *ps, int depth)
{
    jv *v;
    if (depth > MAX_DEPTH) {
        perr(ps, "nested too deeply");
        return NULL;
    }
    skip_ws(ps);
    if (ps->p >= ps->end) {
        perr(ps, "unexpected end of input");
        return NULL;
    }
    switch (*ps->p) {
    case '{': return parse_object(ps, depth);
    case '[': return parse_array(ps, depth);
    case '"': {
        char *s = parse_string(ps);
        if (s == NULL)
            return NULL;
        v = new_value(J_STR);
        v->s = s;
        return v;
    }
    case 't':
        if (literal(ps, "true")) {
            v = new_value(J_BOOL);
            v->b = 1;
            return v;
        }
        break;
    case 'f':
        if (literal(ps, "false"))
            return new_value(J_BOOL);
        break;
    case 'n':
        if (literal(ps, "null"))
            return new_value(J_NULL);
        break;
    default:
        if (*ps->p == '-' || (*ps->p >= '0' && *ps->p <= '9'))
            return parse_number(ps);
    }
    perr(ps, "unexpected character");
    return NULL;
}

jv *json_parse(const char *text, size_t len, char *err, size_t errlen)
{
    parser ps;
    jv *v;

    ps.p = text;
    ps.end = text + len;
    ps.err = err;
    ps.errlen = errlen;
    ps.failed = 0;
    if (err != NULL && errlen > 0)
        err[0] = 0;
    v = parse_value(&ps, 0);
    if (v != NULL) {
        skip_ws(&ps);
        if (ps.p != ps.end) {
            perr(&ps, "trailing characters");
            json_free(v);
            v = NULL;
        }
    }
    return v;
}

void json_free(jv *v)
{
    size_t i;
    if (v == NULL)
        return;
    for (i = 0; i < v->count; i++) {
        json_free(v->items[i]);
        if (v->keys != NULL)
            free(v->keys[i]);
    }
    free(v->items);
    free(v->keys);
    free(v->s);
    free(v);
}

jv *jget(const jv *obj, const char *key)
{
    size_t i;
    if (obj == NULL || obj->type != J_OBJ)
        return NULL;
    for (i = 0; i < obj->count; i++)
        if (strcmp(obj->keys[i], key) == 0)
            return obj->items[i];
    return NULL;
}

/* ------------------------------------------------------------------ writing */

void js_str(sbuf *b, const char *s)
{
    const unsigned char *p = (const unsigned char *)s;
    sb_byte(b, '"');
    for (; *p; p++) {
        switch (*p) {
        case '"': sb_puts(b, "\\\""); break;
        case '\\': sb_puts(b, "\\\\"); break;
        case '\n': sb_puts(b, "\\n"); break;
        case '\r': sb_puts(b, "\\r"); break;
        case '\t': sb_puts(b, "\\t"); break;
        default:
            if (*p < 0x20)
                sb_printf(b, "\\u%04x", *p);
            else
                sb_byte(b, *p);
        }
    }
    sb_byte(b, '"');
}

static void finish_number(sbuf *b, const char *t)
{
    sb_puts(b, t);
    /* serde writes whole numbers as 1.0 */
    if (strpbrk(t, ".eEn") == NULL)
        sb_puts(b, ".0");
}

void js_f64(sbuf *b, double v)
{
    char t[40];
    int p;
    if (!isfinite(v)) {
        sb_puts(b, "null");
        return;
    }
    for (p = 1; p <= 17; p++) {
        snprintf(t, sizeof t, "%.*g", p, v);
        if (strtod(t, NULL) == v)
            break;
    }
    finish_number(b, t);
}

void js_f32(sbuf *b, float v)
{
    char t[40];
    int p;
    if (!isfinite(v)) {
        sb_puts(b, "null");
        return;
    }
    for (p = 1; p <= 9; p++) {
        snprintf(t, sizeof t, "%.*g", p, (double)v);
        if (strtof(t, NULL) == v)
            break;
    }
    finish_number(b, t);
}
