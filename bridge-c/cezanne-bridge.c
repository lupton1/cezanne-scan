/*
 * cezanne-bridge in C, for Mac OS X 10.6 Snow Leopard and later.
 *
 * Shares a SCREEN Cezanne scanner on this Mac's SCSI card over the network, so that
 * Cezanne Scan or cezanne-server on a newer computer can use it ("Bridge" connection).
 * The Rust cezanne-bridge cannot run on 10.6 because Rust needs macOS 10.12 or later.
 * This program speaks the same wire protocol (crates/cezanne-core/src/scsi/tcp.rs):
 *
 *   hello    client: "CZB1" u16 token_len token
 *            server: "CZB1" u8 ok(0)/refused(1) u16 desc_len desc
 *   request  u8 1 | u8 cdb_len cdb | u8 dir (0 none, 1 in, 2 out) | u32 data_len | u32 timeout_ms | out data
 *   response u8 status | u32 transferred | u8 sense_len sense | in data (transferred bytes)
 *
 * All integers are big endian. One client is served at a time and the scanner is opened
 * for each client, so a scanner switched on later is picked up.
 *
 * The scanner is reached through IOKit's SCSITaskDeviceInterface, the same route as the
 * Rust macOS transport (crates/cezanne-core/src/scsi/macos_iokit.rs) and SANE.
 *
 * Firmware and EEPROM writes are always refused, exactly as check_allowed() in
 * crates/cezanne-core/src/scsi/mod.rs: WRITE BUFFER (0x3B), vendor command 0xC9, and SEND
 * with data type 0x8A, 0x8B, 0x8C or the save bit.
 *
 * Build with the Makefile next to this file. Plain C99, CoreFoundation and IOKit only, so
 * it also builds with the gcc 4.2 of Xcode 3.2 on the 10.6 Mac itself.
 */

#include <errno.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/scsi/SCSITaskLib.h>

#define CZB_VERSION "1.0"
#define DEFAULT_PORT 7810
#define MAX_DATA (64u * 1024u * 1024u)
#define SENSE_LEN 18

/* ------------------------------------------------------------------ logging */

static void log_msg(const char *level, const char *fmt, ...)
{
    char stamp[32];
    time_t now = time(NULL);
    struct tm tm;
    va_list ap;

    localtime_r(&now, &tm);
    strftime(stamp, sizeof stamp, "%Y-%m-%d %H:%M:%S", &tm);
    fprintf(stderr, "[%s %s] ", stamp, level);
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
}

#define log_info(...) log_msg("INFO ", __VA_ARGS__)
#define log_warn(...) log_msg("WARN ", __VA_ARGS__)
#define log_error(...) log_msg("ERROR", __VA_ARGS__)

/* ------------------------------------------------------------------ safety */

/* Mirrors check_allowed() in cezanne-core. Returns NULL when the command may be sent. */
static const char *blocked_reason(const uint8_t *cdb, size_t len)
{
    uint8_t op;

    if (len == 0)
        return "empty CDB";
    op = cdb[0];
    if (op == 0x3B)
        return "WRITE BUFFER (firmware download)";
    if (op == 0xC9)
        return "vendor maintenance write (0xC9)";
    if (op == 0x2A) {
        uint8_t dtc = len > 2 ? cdb[2] : 0;
        int save = len > 4 ? (cdb[4] & 1) : 0;
        if (dtc == 0x8A)
            return "SEND memory switches (0x8A)";
        if (dtc == 0x8B)
            return "SEND factory ROM data (0x8B)";
        if (dtc == 0x8C)
            return "SEND serial number (0x8C)";
        if (save)
            return "SEND with the save to EEPROM bit";
    }
    return NULL;
}

/* ------------------------------------------------------------------ transports */

enum { DIR_NONE = 0, DIR_IN = 1, DIR_OUT = 2 };

struct completion {
    uint8_t status;
    uint32_t transferred;
    uint8_t sense[SENSE_LEN];
    uint8_t sense_len;
};

struct transport {
    /* Returns 0 when the command ran (whatever its SCSI status), -1 on a local failure. */
    int (*execute)(struct transport *t, const uint8_t *cdb, uint8_t cdb_len, int dir,
                   uint8_t *buf, uint32_t len, uint32_t timeout_ms, struct completion *c);
    void (*close)(struct transport *t);
    char desc[256];
};

/* ---------------------------------------------------------- socket helpers */

static int read_full(int fd, void *buf, size_t n)
{
    uint8_t *p = buf;
    while (n > 0) {
        ssize_t r = read(fd, p, n);
        if (r == 0)
            return -1;
        if (r < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static int write_full(int fd, const void *buf, size_t n)
{
    const uint8_t *p = buf;
    while (n > 0) {
        ssize_t r = write(fd, p, n);
        if (r < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static uint32_t get_u32(const uint8_t *b)
{
    return ((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) | ((uint32_t)b[2] << 8) | b[3];
}

static void put_u32(uint8_t *b, uint32_t v)
{
    b[0] = (uint8_t)(v >> 24);
    b[1] = (uint8_t)(v >> 16);
    b[2] = (uint8_t)(v >> 8);
    b[3] = (uint8_t)v;
}

static int write_bytes16(int fd, const void *b, size_t len)
{
    uint8_t l[2];
    if (len > 0xFFFF)
        len = 0xFFFF;
    l[0] = (uint8_t)(len >> 8);
    l[1] = (uint8_t)len;
    if (write_full(fd, l, 2) < 0)
        return -1;
    return write_full(fd, b, len);
}

/* Reads a u16 length and that many bytes into a new NUL terminated buffer. */
static char *read_bytes16(int fd, size_t *out_len)
{
    uint8_t l[2];
    size_t len;
    char *s;

    if (read_full(fd, l, 2) < 0)
        return NULL;
    len = ((size_t)l[0] << 8) | l[1];
    s = malloc(len + 1);
    if (s == NULL)
        return NULL;
    if (read_full(fd, s, len) < 0) {
        free(s);
        return NULL;
    }
    s[len] = 0;
    *out_len = len;
    return s;
}

/* ------------------------------------------------------------------ IOKit */

struct iokit_transport {
    struct transport base;
    SCSITaskDeviceInterface **device;
};

static int cf_string(CFTypeRef v, char *out, size_t size)
{
    size_t n;
    out[0] = 0;
    if (v == NULL || CFGetTypeID(v) != CFStringGetTypeID())
        return -1;
    if (!CFStringGetCString((CFStringRef)v, out, (CFIndex)size, kCFStringEncodingUTF8))
        return -1;
    /* inquiry strings are space padded */
    n = strlen(out);
    while (n > 0 && out[n - 1] == ' ')
        out[--n] = 0;
    return 0;
}

/* Reads a string property from the device or the nearest parent that has it. */
static void registry_string(io_service_t s, const char *key, char *out, size_t size)
{
    CFStringRef k = CFStringCreateWithCString(kCFAllocatorDefault, key, kCFStringEncodingUTF8);
    CFTypeRef v = IORegistryEntrySearchCFProperty(s, kIOServicePlane, k, kCFAllocatorDefault,
                                                  kIORegistryIterateRecursively | kIORegistryIterateParents);
    cf_string(v, out, size);
    if (v != NULL)
        CFRelease(v);
    CFRelease(k);
}

static int registry_number(io_service_t s, const char *key)
{
    int n = -1;
    CFStringRef k = CFStringCreateWithCString(kCFAllocatorDefault, key, kCFStringEncodingUTF8);
    CFTypeRef v = IORegistryEntrySearchCFProperty(s, kIOServicePlane, k, kCFAllocatorDefault,
                                                  kIORegistryIterateRecursively | kIORegistryIterateParents);
    if (v != NULL) {
        if (CFGetTypeID(v) == CFNumberGetTypeID())
            CFNumberGetValue((CFNumberRef)v, kCFNumberIntType, &n);
        CFRelease(v);
    }
    CFRelease(k);
    return n;
}

struct device_info {
    uint64_t id;
    char vendor[64];
    char product[64];
    char revision[32];
    int type;
};

static int is_supported(const struct device_info *d)
{
    return strcmp(d->vendor, "SCREEN") == 0 &&
           (strncmp(d->product, "FT-S5", 5) == 0 || strncmp(d->product, "FT-S3", 5) == 0);
}

static void device_info(io_service_t s, struct device_info *d)
{
    memset(d, 0, sizeof *d);
    IORegistryEntryGetRegistryEntryID(s, &d->id);
    registry_string(s, "Vendor Identification", d->vendor, sizeof d->vendor);
    registry_string(s, "Product Identification", d->product, sizeof d->product);
    registry_string(s, "Product Revision Level", d->revision, sizeof d->revision);
    d->type = registry_number(s, "Peripheral Device Type");
}

/* All SCSI devices that user space may drive, the ones without a kernel driver. Recent
 * macOS versions return no iterator at all (0) when nothing matches. */
static kern_return_t user_client_devices(io_iterator_t *iter)
{
    CFMutableDictionaryRef outer = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks,
                                                             &kCFTypeDictionaryValueCallBacks);
    CFMutableDictionaryRef inner = CFDictionaryCreateMutable(kCFAllocatorDefault, 0, &kCFTypeDictionaryKeyCallBacks,
                                                             &kCFTypeDictionaryValueCallBacks);
    CFDictionarySetValue(inner, CFSTR(kIOPropertySCSITaskDeviceCategory), CFSTR(kIOPropertySCSITaskUserClientDevice));
    CFDictionarySetValue(outer, CFSTR(kIOPropertyMatchKey), inner);
    CFRelease(inner);
    *iter = 0;
    /* consumes outer, port 0 is the default main port on every macOS version */
    return IOServiceGetMatchingServices(0, outer, iter);
}

static int list_devices(void)
{
    io_iterator_t iter;
    io_service_t s;
    int count = 0;
    kern_return_t kr = user_client_devices(&iter);

    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "IOKit device search failed (0x%x)\n", kr);
        return 1;
    }
    while (iter != 0 && (s = IOIteratorNext(iter)) != 0) {
        struct device_info d;
        device_info(s, &d);
        printf("iokit:%llu  %-8s %-16s %-4s type %d%s\n", (unsigned long long)d.id, d.vendor, d.product,
               d.revision, d.type, is_supported(&d) ? "  (supported scanner)" : "");
        count++;
        IOObjectRelease(s);
    }
    if (iter != 0)
        IOObjectRelease(iter);
    if (count == 0)
        printf("No SCSI devices without a driver were found. Check the SCSI card, its driver, the cable "
               "and termination, and that the scanner was switched on before the Mac.\n");
    return 0;
}

/* Finds the device with this registry id, or the first supported scanner when id is 0. */
static io_service_t find_device(uint64_t want, struct device_info *out)
{
    io_iterator_t iter;
    io_service_t s, found = 0;

    if (user_client_devices(&iter) != KERN_SUCCESS || iter == 0)
        return 0;
    while (found == 0 && (s = IOIteratorNext(iter)) != 0) {
        struct device_info d;
        device_info(s, &d);
        if (want != 0 ? d.id == want : is_supported(&d)) {
            found = s;
            *out = d;
        } else {
            IOObjectRelease(s);
        }
    }
    IOObjectRelease(iter);
    return found;
}

static int iokit_execute(struct transport *t, const uint8_t *cdb, uint8_t cdb_len, int dir, uint8_t *buf,
                         uint32_t len, uint32_t timeout_ms, struct completion *c)
{
    struct iokit_transport *it = (struct iokit_transport *)t;
    SCSITaskInterface **task;
    SCSITaskSGElement range;
    SCSI_Sense_Data sense;
    SCSITaskStatus status = kSCSITaskStatus_No_Status;
    UInt64 transferred = 0;
    UInt8 cdb_copy[16];
    UInt8 direction;
    IOReturn r;
    int result = -1;

    if (cdb_len > 16) {
        log_warn("CDB longer than 16 bytes");
        return -1;
    }
    memcpy(cdb_copy, cdb, cdb_len);
    direction = dir == DIR_IN ? kSCSIDataTransfer_FromTargetToInitiator
              : dir == DIR_OUT ? kSCSIDataTransfer_FromInitiatorToTarget
                               : kSCSIDataTransfer_NoDataTransfer;

    task = (*it->device)->CreateSCSITask(it->device);
    if (task == NULL) {
        log_warn("cannot create a SCSI task");
        return -1;
    }
    r = (*task)->SetCommandDescriptorBlock(task, cdb_copy, cdb_len);
    if (r != kIOReturnSuccess) {
        log_warn("SetCommandDescriptorBlock failed (0x%x)", r);
        goto done;
    }
    memset(&range, 0, sizeof range);
    range.address = (uintptr_t)buf;
    range.length = len;
    r = (*task)->SetScatterGatherEntries(task, &range, len > 0 ? 1 : 0, len, direction);
    if (r != kIOReturnSuccess) {
        log_warn("SetScatterGatherEntries failed (0x%x)", r);
        goto done;
    }
    (*task)->SetTimeoutDuration(task, timeout_ms);
    memset(&sense, 0, sizeof sense);
    r = (*task)->ExecuteTaskSync(task, &sense, &status, &transferred);
    if (r != kIOReturnSuccess) {
        log_warn("SCSI task 0x%02X failed (0x%x)", cdb[0], r);
        goto done;
    }
    c->status = (uint8_t)status;
    c->transferred = transferred > len ? len : (uint32_t)transferred;
    c->sense_len = 0;
    if (status != kSCSITaskStatus_GOOD) {
        const uint8_t *sb = (const uint8_t *)&sense;
        uint8_t code = sb[0] & 0x7F;
        if (code == 0x70 || code == 0x71) {
            memcpy(c->sense, sb, SENSE_LEN);
            c->sense_len = SENSE_LEN;
        }
    }
    result = 0;
done:
    (*task)->Release(task);
    return result;
}

static void iokit_close(struct transport *t)
{
    struct iokit_transport *it = (struct iokit_transport *)t;
    (*it->device)->ReleaseExclusiveAccess(it->device);
    (*it->device)->Release(it->device);
    free(it);
}

static struct transport *iokit_open(uint64_t want)
{
    struct device_info d;
    io_service_t service = find_device(want, &d);
    IOCFPlugInInterface **plugin = NULL;
    SCSITaskDeviceInterface **device = NULL;
    struct iokit_transport *it;
    SInt32 score = 0;
    IOReturn kr;
    HRESULT hr;

    if (service == 0) {
        if (want != 0)
            log_error("no IOKit device with id %llu, run cezanne-bridge --list", (unsigned long long)want);
        else
            log_error("no SCREEN scanner found on the SCSI bus, run cezanne-bridge --list");
        return NULL;
    }
    kr = IOCreatePlugInInterfaceForService(service, kIOSCSITaskDeviceUserClientTypeID, kIOCFPlugInInterfaceID,
                                           &plugin, &score);
    IOObjectRelease(service);
    if (kr != kIOReturnSuccess || plugin == NULL) {
        log_error("cannot open the SCSI user client (0x%x)", kr);
        return NULL;
    }
    hr = (*plugin)->QueryInterface(plugin, CFUUIDGetUUIDBytes(kIOSCSITaskDeviceInterfaceID), (LPVOID *)&device);
    IODestroyPlugInInterface(plugin);
    if (hr != S_OK || device == NULL) {
        log_error("SCSITaskDeviceInterface not available (0x%x)", (int)hr);
        return NULL;
    }
    kr = (*device)->ObtainExclusiveAccess(device);
    if (kr != kIOReturnSuccess) {
        log_error("the scanner is in use by another program (0x%x). Quit other scanning software and try again",
                  kr);
        (*device)->Release(device);
        return NULL;
    }
    it = calloc(1, sizeof *it);
    if (it == NULL) {
        (*device)->ReleaseExclusiveAccess(device);
        (*device)->Release(device);
        return NULL;
    }
    it->device = device;
    it->base.execute = iokit_execute;
    it->base.close = iokit_close;
    snprintf(it->base.desc, sizeof it->base.desc, "IOKit iokit:%llu %s %s %s on Mac OS X",
             (unsigned long long)d.id, d.vendor, d.product, d.revision);
    return &it->base;
}

#ifdef CZB_RELAY
/* ------------------------------------------------------------------ relay (tests only)
 *
 * Forwards every command to another bridge, for example the Rust cezanne-bridge --sim.
 * Only built with -DCZB_RELAY (make relay) to test this program's protocol handling
 * end to end on a Mac without the scanner. Never part of the release build.
 */

struct relay_transport {
    struct transport base;
    int fd;
};

static int connect_tcp(const char *hostport, int default_port);

static int relay_execute(struct transport *t, const uint8_t *cdb, uint8_t cdb_len, int dir, uint8_t *buf,
                         uint32_t len, uint32_t timeout_ms, struct completion *c)
{
    struct relay_transport *rt = (struct relay_transport *)t;
    uint8_t h[3 + 16 + 9], r[6];
    size_t n = 0;

    h[n++] = 1;
    h[n++] = cdb_len;
    memcpy(h + n, cdb, cdb_len);
    n += cdb_len;
    h[n++] = (uint8_t)dir;
    put_u32(h + n, len);
    n += 4;
    put_u32(h + n, timeout_ms);
    n += 4;
    if (write_full(rt->fd, h, n) < 0 || (dir == DIR_OUT && write_full(rt->fd, buf, len) < 0))
        return -1;
    if (read_full(rt->fd, r, 6) < 0)
        return -1;
    c->status = r[0];
    c->transferred = get_u32(r + 1);
    c->sense_len = r[5] > SENSE_LEN ? SENSE_LEN : r[5];
    if (read_full(rt->fd, c->sense, c->sense_len) < 0)
        return -1;
    if (r[5] > SENSE_LEN) {
        uint8_t skip[256];
        if (read_full(rt->fd, skip, r[5] - SENSE_LEN) < 0)
            return -1;
    }
    if (dir == DIR_IN) {
        if (c->transferred > len || read_full(rt->fd, buf, c->transferred) < 0)
            return -1;
    }
    return 0;
}

static void relay_close(struct transport *t)
{
    struct relay_transport *rt = (struct relay_transport *)t;
    close(rt->fd);
    free(rt);
}

static struct transport *relay_open(const char *hostport)
{
    struct relay_transport *rt;
    uint8_t magic[4], ok;
    size_t dlen;
    char *desc;
    int fd = connect_tcp(hostport, DEFAULT_PORT);

    if (fd < 0) {
        log_error("cannot reach the bridge at %s", hostport);
        return NULL;
    }
    if (write_full(fd, "CZB1", 4) < 0 || write_bytes16(fd, "", 0) < 0 || read_full(fd, magic, 4) < 0 ||
        memcmp(magic, "CZB1", 4) != 0 || read_full(fd, &ok, 1) < 0 || (desc = read_bytes16(fd, &dlen)) == NULL) {
        log_error("%s is not a cezanne-bridge", hostport);
        close(fd);
        return NULL;
    }
    if (ok != 0) {
        log_error("bridge at %s refused: %s", hostport, desc);
        free(desc);
        close(fd);
        return NULL;
    }
    rt = calloc(1, sizeof *rt);
    if (rt == NULL) {
        free(desc);
        close(fd);
        return NULL;
    }
    rt->fd = fd;
    rt->base.execute = relay_execute;
    rt->base.close = relay_close;
    snprintf(rt->base.desc, sizeof rt->base.desc, "relay %s (%s)", hostport, desc);
    free(desc);
    return &rt->base;
}

static int connect_tcp(const char *hostport, int default_port)
{
    char host[256], port[16];
    const char *colon = strrchr(hostport, ':');
    struct addrinfo hints, *res, *ai;
    int fd = -1;

    if (colon != NULL) {
        snprintf(host, sizeof host, "%.*s", (int)(colon - hostport), hostport);
        snprintf(port, sizeof port, "%s", colon + 1);
    } else {
        snprintf(host, sizeof host, "%s", hostport);
        snprintf(port, sizeof port, "%d", default_port);
    }
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo(host, port, &hints, &res) != 0)
        return -1;
    for (ai = res; ai != NULL; ai = ai->ai_next) {
        fd = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (fd < 0)
            continue;
        if (connect(fd, ai->ai_addr, ai->ai_addrlen) == 0)
            break;
        close(fd);
        fd = -1;
    }
    freeaddrinfo(res);
    return fd;
}
#endif

/* ------------------------------------------------------------------ serving */

/* Serve one client until it disconnects. Returns 0 on a clean disconnect. */
static int serve(int fd, struct transport *t, const char *token)
{
    uint8_t magic[4], ok;
    size_t tlen;
    char *client_token;
    uint8_t *buf = NULL;
    size_t buf_cap = 0;
    int result = -1;

    if (read_full(fd, magic, 4) < 0 || memcmp(magic, "CZB1", 4) != 0) {
        log_warn("client did not send the bridge greeting");
        return -1;
    }
    client_token = read_bytes16(fd, &tlen);
    if (client_token == NULL)
        return -1;
    if (write_full(fd, "CZB1", 4) < 0) {
        free(client_token);
        return -1;
    }
    if (tlen != strlen(token) || memcmp(client_token, token, tlen) != 0) {
        free(client_token);
        ok = 1;
        write_full(fd, &ok, 1);
        write_bytes16(fd, "wrong token", 11);
        log_warn("client sent a wrong token");
        return -1;
    }
    free(client_token);
    ok = 0;
    if (write_full(fd, &ok, 1) < 0 || write_bytes16(fd, t->desc, strlen(t->desc)) < 0)
        return -1;

    for (;;) {
        uint8_t kind, cdb_len, cdb[255], dir, h[8], resp[6];
        uint32_t len, timeout_ms, transferred;
        struct completion c;
        const char *why;

        if (read_full(fd, &kind, 1) < 0) {
            result = 0; /* the client closed the connection */
            break;
        }
        if (kind != 1) {
            log_warn("unknown request kind %u", kind);
            break;
        }
        if (read_full(fd, &cdb_len, 1) < 0 || read_full(fd, cdb, cdb_len) < 0 || read_full(fd, &dir, 1) < 0 ||
            read_full(fd, h, 8) < 0)
            break;
        len = get_u32(h);
        timeout_ms = get_u32(h + 4);
        if (len > MAX_DATA) {
            log_warn("data phase too large");
            break;
        }
        if (len > buf_cap) {
            uint8_t *nb = realloc(buf, len);
            if (nb == NULL) {
                log_error("out of memory for %u bytes", len);
                break;
            }
            buf = nb;
            buf_cap = len;
        }
        if (dir == DIR_OUT && read_full(fd, buf, len) < 0)
            break;
        if (dir != DIR_OUT && len > 0)
            memset(buf, 0, len);

        memset(&c, 0, sizeof c);
        why = blocked_reason(cdb, cdb_len);
        if (why != NULL)
            log_warn("refused command from client: %s", why);
        if (why != NULL ||
            t->execute(t, cdb, cdb_len, dir > DIR_OUT ? DIR_OUT : dir, buf, len, timeout_ms, &c) < 0) {
            if (why == NULL)
                log_warn("command 0x%02X failed locally", cdb_len > 0 ? cdb[0] : 0);
            /* CHECK CONDITION, ILLEGAL REQUEST, INVALID FIELD IN PARAMETER LIST, as the Rust bridge */
            memset(&c, 0, sizeof c);
            c.status = 0x02;
            c.sense[0] = 0x70;
            c.sense[2] = 0x05;
            c.sense[7] = 0x0B;
            c.sense[12] = 0x26;
            c.sense[13] = 0x02;
            c.sense_len = SENSE_LEN;
        }

        transferred = dir == DIR_IN && c.transferred > len ? len : c.transferred;
        resp[0] = c.status;
        put_u32(resp + 1, transferred);
        resp[5] = c.sense_len;
        if (write_full(fd, resp, 6) < 0 || write_full(fd, c.sense, c.sense_len) < 0)
            break;
        if (dir == DIR_IN && write_full(fd, buf, transferred) < 0)
            break;
    }
    free(buf);
    return result;
}

static int listen_on(const char *addr)
{
    char host[256], port[16];
    const char *colon = strrchr(addr, ':');
    struct addrinfo hints, *res;
    int fd, one = 1;

    if (colon == NULL) {
        fprintf(stderr, "--listen needs HOST:PORT, for example 0.0.0.0:%d\n", DEFAULT_PORT);
        return -1;
    }
    snprintf(host, sizeof host, "%.*s", (int)(colon - addr), addr);
    snprintf(port, sizeof port, "%s", colon + 1);
    memset(&hints, 0, sizeof hints);
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_PASSIVE;
    if (getaddrinfo(host[0] ? host : NULL, port, &hints, &res) != 0) {
        fprintf(stderr, "cannot listen on %s: unknown address\n", addr);
        return -1;
    }
    fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0 || setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one) < 0 ||
        bind(fd, res->ai_addr, res->ai_addrlen) < 0 || listen(fd, 4) < 0) {
        fprintf(stderr, "cannot listen on %s: %s\n", addr, strerror(errno));
        freeaddrinfo(res);
        if (fd >= 0)
            close(fd);
        return -1;
    }
    freeaddrinfo(res);
    return fd;
}

static void usage(void)
{
    printf("cezanne-bridge (C) %s: share a SCREEN Cezanne scanner over the network\n"
           "\n"
           "Usage: cezanne-bridge [--listen HOST:PORT] [--token SECRET] [--device iokit:ID]\n"
           "       cezanne-bridge --list\n"
           "\n"
           "  --listen HOST:PORT  address to listen on (default 127.0.0.1:%d). Use 0.0.0.0:%d to\n"
           "                      accept other computers on your network\n"
           "  --token SECRET      shared secret clients must send, also read from CEZANNE_BRIDGE_TOKEN.\n"
           "                      Strongly recommended when listening on the network\n"
           "  --device iokit:ID   the scanner to use (default: the first SCREEN scanner found)\n"
           "  --list              list the SCSI devices this Mac can reach and exit\n"
#ifdef CZB_RELAY
           "  --relay HOST:PORT   test build only: forward to another bridge instead of the scanner\n"
#endif
           "  --version           print the version\n",
           CZB_VERSION, DEFAULT_PORT, DEFAULT_PORT);
}

int main(int argc, char **argv)
{
    const char *listen_addr = "127.0.0.1:7810";
    const char *token = getenv("CEZANNE_BRIDGE_TOKEN");
    uint64_t device_id = 0;
#ifdef CZB_RELAY
    const char *relay = NULL;
#endif
    int i, lfd;

    if (token == NULL)
        token = "";
    for (i = 1; i < argc; i++) {
        const char *a = argv[i];
        const char *next = i + 1 < argc ? argv[i + 1] : NULL;
        if (strcmp(a, "--help") == 0 || strcmp(a, "-h") == 0) {
            usage();
            return 0;
        } else if (strcmp(a, "--version") == 0 || strcmp(a, "-V") == 0) {
            printf("cezanne-bridge (C) %s\n", CZB_VERSION);
            return 0;
        } else if (strcmp(a, "--list") == 0) {
            return list_devices();
        } else if (strcmp(a, "--listen") == 0 && next != NULL) {
            listen_addr = next;
            i++;
        } else if (strcmp(a, "--token") == 0 && next != NULL) {
            token = next;
            i++;
        } else if (strcmp(a, "--device") == 0 && next != NULL) {
            char *end = NULL;
            if (strncmp(next, "iokit:", 6) == 0)
                device_id = strtoull(next + 6, &end, 10);
            if (device_id == 0 || end == NULL || *end != 0) {
                fprintf(stderr, "expected --device iokit:<registry id>, got %s\n", next);
                return 2;
            }
            i++;
#ifdef CZB_RELAY
        } else if (strcmp(a, "--relay") == 0 && next != NULL) {
            relay = next;
            i++;
#endif
        } else {
            fprintf(stderr, "unknown or incomplete option %s, see --help\n", a);
            return 2;
        }
    }

    signal(SIGPIPE, SIG_IGN);
    if (token[0] == 0 && strncmp(listen_addr, "127.", 4) != 0 && strncmp(listen_addr, "localhost", 9) != 0)
        log_warn("listening on the network without --token: anyone on your network can use the scanner");
    lfd = listen_on(listen_addr);
    if (lfd < 0)
        return 1;
    log_info("cezanne-bridge (C) %s listening on %s", CZB_VERSION, listen_addr);

    for (;;) {
        struct sockaddr_storage peer;
        socklen_t plen = sizeof peer;
        char name[NI_MAXHOST + NI_MAXSERV + 2] = "?", h[NI_MAXHOST], p[NI_MAXSERV];
        struct transport *t;
        int one = 1;
        int cfd = accept(lfd, (struct sockaddr *)&peer, &plen);

        if (cfd < 0) {
            if (errno != EINTR)
                log_warn("accept failed: %s", strerror(errno));
            continue;
        }
        if (getnameinfo((struct sockaddr *)&peer, plen, h, sizeof h, p, sizeof p, NI_NUMERICHOST | NI_NUMERICSERV) ==
            0)
            snprintf(name, sizeof name, "%s:%s", h, p);
        setsockopt(cfd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);

        /* open the device per client, so a scanner switched on later is picked up */
#ifdef CZB_RELAY
        t = relay != NULL ? relay_open(relay) : iokit_open(device_id);
#else
        t = iokit_open(device_id);
#endif
        if (t == NULL) {
            log_error("cannot open the scanner for %s", name);
            close(cfd);
            continue;
        }
        log_info("client %s connected, serving %s", name, t->desc);
        if (serve(cfd, t, token) == 0)
            log_info("client %s disconnected", name);
        else
            log_warn("client %s: connection ended with an error", name);
        t->close(t);
        close(cfd);
    }
}
