/*
 * SCSI transports: IOKit on the Mac (crates/cezanne-core/src/scsi/macos_iokit.rs) and the
 * cezanne-bridge TCP client (scsi/tcp.rs), plus the safety check of scsi/mod.rs.
 */
#include "cz.h"
#include "plat.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>

/* ------------------------------------------------------------------ safety */

/* Mirrors check_allowed() in cezanne-core. These commands write firmware or factory data
 * into the scanner and are never sent, whatever asks for them. */
const char *cz_blocked_reason(const uint8_t *cdb, size_t len)
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

/* ------------------------------------------------------------------ sense */

static const char *key_name(uint8_t key, char *tmp, size_t size)
{
    switch (key & 0x0F) {
    case 0x0: return "NoSense";
    case 0x1: return "RecoveredError";
    case 0x2: return "NotReady";
    case 0x3: return "MediumError";
    case 0x4: return "HardwareError";
    case 0x5: return "IllegalRequest";
    case 0x6: return "UnitAttention";
    case 0x7: return "DataProtect";
    case 0xB: return "AbortedCommand";
    default:
        snprintf(tmp, size, "Other(%u)", key & 0x0F);
        return tmp;
    }
}

static const char *meaning(uint8_t key, uint8_t asc)
{
    switch (key & 0x0F) {
    case SK_NO_SENSE: return "no error";
    case SK_NOT_READY: return asc == 0x04 ? "scanner is warming up or busy" : "scanner not ready";
    case SK_ILLEGAL:
        if (asc == 0x20)
            return "command not supported by this scanner";
        if (asc == 0x24)
            return "invalid field in the command";
        if (asc == 0x26)
            return "invalid parameter (window, table or data type)";
        if (asc == 0x2C)
            return "command sequence error";
        return "unknown condition";
    case SK_HARDWARE:
        if (asc == 0x84)
            return "EEPROM access failed";
        if (asc == 0x88)
            return "scan engine error";
        return "hardware error";
    case SK_UNIT_ATTENTION: return "scanner was reset or its state changed";
    case SK_ABORTED: return "command aborted";
    default: return "unknown condition";
    }
}

void cz_sense_text(uint8_t key, uint8_t asc, uint8_t ascq, char *out, size_t size)
{
    char tmp[16];
    snprintf(out, size, "%s ASC 0x%02X ASCQ 0x%02X (%s)", key_name(key, tmp, sizeof tmp), asc, ascq,
             meaning(key, asc));
}

/* Sense::parse: fixed format sense data only */
static int sense_ok(const uint8_t *b, size_t n)
{
    uint8_t code;
    if (n < 3)
        return 0;
    code = b[0] & 0x7F;
    return code == 0x70 || code == 0x71;
}

int cz_scsi_run(cz_transport *t, const uint8_t *cdb, size_t cdb_len, int dir, uint8_t *buf, size_t len,
                unsigned timeout_ms, size_t *transferred, cz_err *e)
{
    cz_completion c;
    const char *why = cz_blocked_reason(cdb, cdb_len);

    if (why != NULL)
        return cz_fail(e, cdb_len == 0 ? E_INVALID : E_BLOCKED, "%s", why);
    memset(&c, 0, sizeof c);
    if (t->execute(t, cdb, cdb_len, dir, buf, len, timeout_ms, &c, e) < 0)
        return -1;
    if (c.status == 0) {
        if (transferred != NULL)
            *transferred = c.transferred;
        return 0;
    }
    cz_fail(e, E_SCSI, "");
    e->opcode = cdb[0];
    e->status = c.status;
    if (c.has_sense) {
        char st[160];
        e->has_sense = 1;
        e->sense_key = c.sense[2] & 0x0F;
        e->asc = c.sense[12];
        e->ascq = c.sense[13];
        cz_sense_text(e->sense_key, e->asc, e->ascq, st, sizeof st);
        snprintf(e->msg, sizeof e->msg, "SCSI command 0x%02X failed with status 0x%02X: %s", e->opcode,
                 e->status, st);
    } else {
        snprintf(e->msg, sizeof e->msg, "SCSI command 0x%02X failed with status 0x%02X", e->opcode, e->status);
    }
    return -1;
}

/* ------------------------------------------------------------------ bridge client */

typedef struct {
    cz_transport base;
    sock_t s;
} tcp_transport;

static void put_u32(uint8_t *b, uint32_t v)
{
    b[0] = (uint8_t)(v >> 24);
    b[1] = (uint8_t)(v >> 16);
    b[2] = (uint8_t)(v >> 8);
    b[3] = (uint8_t)v;
}

static uint32_t get_u32(const uint8_t *b)
{
    return ((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) | ((uint32_t)b[2] << 8) | b[3];
}

static int tcp_execute(cz_transport *t, const uint8_t *cdb, size_t cdb_len, int dir, uint8_t *buf, size_t len,
                       unsigned timeout_ms, cz_completion *c, cz_err *e)
{
    tcp_transport *tt = (tcp_transport *)t;
    uint8_t req[16 + 255], resp[6], sense[255];
    size_t n = 0;
    uint32_t transferred;
    const char *why = cz_blocked_reason(cdb, cdb_len);

    if (why != NULL)
        return cz_fail(e, E_BLOCKED, "%s", why);
    if (cdb_len > 255 || len > 0xFFFFFFFFu)
        return cz_fail(e, E_INVALID, "command too long for the bridge");
    sock_timeouts(tt->s, timeout_ms + 10000, timeout_ms + 10000);
    req[n++] = 1;
    req[n++] = (uint8_t)cdb_len;
    memcpy(req + n, cdb, cdb_len);
    n += cdb_len;
    req[n++] = (uint8_t)dir;
    put_u32(req + n, (uint32_t)len);
    n += 4;
    put_u32(req + n, timeout_ms);
    n += 4;
    if (sock_write_full(tt->s, req, n) < 0 || (dir == DIR_OUT && len > 0 && sock_write_full(tt->s, buf, len) < 0))
        return cz_fail(e, E_IO, "lost the connection to the bridge");
    if (sock_read_full(tt->s, resp, 6) < 0)
        return cz_fail(e, E_IO, "lost the connection to the bridge");
    transferred = get_u32(resp + 1);
    if (resp[5] > 0 && sock_read_full(tt->s, sense, resp[5]) < 0)
        return cz_fail(e, E_IO, "lost the connection to the bridge");
    c->status = resp[0];
    c->transferred = transferred;
    c->has_sense = sense_ok(sense, resp[5]);
    if (c->has_sense) {
        memset(c->sense, 0, SENSE_LEN);
        memcpy(c->sense, sense, resp[5] > SENSE_LEN ? SENSE_LEN : resp[5]);
    }
    if (dir == DIR_IN) {
        if (transferred > len)
            return cz_fail(e, E_PROTOCOL, "bridge sent more data than requested");
        if (transferred > 0 && sock_read_full(tt->s, buf, transferred) < 0)
            return cz_fail(e, E_IO, "lost the connection to the bridge");
    }
    return 0;
}

static void tcp_close(cz_transport *t)
{
    tcp_transport *tt = (tcp_transport *)t;
    sock_close(tt->s);
    free(tt);
}

static int write_bytes16(sock_t s, const char *b, size_t len)
{
    uint8_t l[2];
    l[0] = (uint8_t)(len >> 8);
    l[1] = (uint8_t)len;
    if (sock_write_full(s, l, 2) < 0)
        return -1;
    return len ? sock_write_full(s, b, len) : 0;
}

cz_transport *cz_tcp_connect(const char *addr_in, const char *token, cz_err *e)
{
    char addr[300], host[256], port[16], desc[256];
    const char *colon;
    uint8_t magic[4], ok, l[2];
    size_t dlen;
    sock_t s;
    tcp_transport *tt;

    /* the Rust client adds the default port when the address has no colon */
    if (strchr(addr_in, ':') != NULL)
        snprintf(addr, sizeof addr, "%s", addr_in);
    else
        snprintf(addr, sizeof addr, "%s:%d", addr_in, CZ_BRIDGE_PORT);
    if (strlen(token) > 0xFFFF) {
        cz_fail(e, E_PROTOCOL, "string too long");
        return NULL;
    }
    colon = strrchr(addr, ':');
    snprintf(host, sizeof host, "%.*s", (int)(colon - addr), addr);
    snprintf(port, sizeof port, "%s", colon + 1);
    /* [::1]:7810 style */
    if (host[0] == '[' && host[strlen(host) - 1] == ']') {
        memmove(host, host + 1, strlen(host));
        host[strlen(host) - 1] = 0;
    }
    s = sock_connect(host, port, 5000);
    if (s == BAD_SOCK) {
        cz_fail(e, E_IO, "cannot reach the bridge at %s", addr);
        return NULL;
    }
    sock_nodelay(s);
    sock_timeouts(s, 15000, 15000);
    if (sock_write_full(s, "CZB1", 4) < 0 || write_bytes16(s, token, strlen(token)) < 0 ||
        sock_read_full(s, magic, 4) < 0) {
        sock_close(s);
        cz_fail(e, E_IO, "the bridge at %s closed the connection", addr);
        return NULL;
    }
    if (memcmp(magic, "CZB1", 4) != 0) {
        sock_close(s);
        cz_fail(e, E_PROTOCOL, "not a cezanne-bridge");
        return NULL;
    }
    if (sock_read_full(s, &ok, 1) < 0 || sock_read_full(s, l, 2) < 0) {
        sock_close(s);
        cz_fail(e, E_IO, "the bridge at %s closed the connection", addr);
        return NULL;
    }
    dlen = ((size_t)l[0] << 8) | l[1];
    {
        char *d = xmalloc(dlen + 1);
        if (dlen > 0 && sock_read_full(s, d, dlen) < 0) {
            free(d);
            sock_close(s);
            cz_fail(e, E_IO, "the bridge at %s closed the connection", addr);
            return NULL;
        }
        d[dlen] = 0;
        snprintf(desc, sizeof desc, "%s", d);
        free(d);
    }
    if (ok != 0) {
        sock_close(s);
        cz_fail(e, E_PROTOCOL, "bridge refused the connection: %s", desc);
        return NULL;
    }
    tt = xcalloc(1, sizeof *tt);
    tt->s = s;
    tt->base.execute = tcp_execute;
    tt->base.close = tcp_close;
    snprintf(tt->base.desc, sizeof tt->base.desc, "bridge %s (%s)", addr, desc);
    return &tt->base;
}

/* ------------------------------------------------------------------ IOKit */

#ifdef __APPLE__
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/scsi/SCSITaskLib.h>

typedef struct {
    cz_transport base;
    IOCFPlugInInterface **plugin;
    SCSITaskDeviceInterface **device;
} iokit_transport;

static void cf_string(CFTypeRef v, char *out, size_t size)
{
    size_t n;
    out[0] = 0;
    if (v == NULL || CFGetTypeID(v) != CFStringGetTypeID())
        return;
    if (!CFStringGetCString((CFStringRef)v, out, (CFIndex)size, kCFStringEncodingUTF8)) {
        out[0] = 0;
        return;
    }
    n = strlen(out);
    while (n > 0 && out[n - 1] == ' ')
        out[--n] = 0;
}

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

static int is_supported(const char *vendor, const char *product)
{
    return strcmp(vendor, "SCREEN") == 0 && (strncmp(product, "FT-S5", 5) == 0 || strncmp(product, "FT-S3", 5) == 0);
}

/* SCSI devices user space may drive (those without a kernel driver) */
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
    return IOServiceGetMatchingServices(0, outer, iter);
}

typedef struct {
    uint64_t id;
    char vendor[64], product[64], revision[32];
    int type;
} reg_device;

static void read_device(io_service_t s, reg_device *d)
{
    memset(d, 0, sizeof *d);
    IORegistryEntryGetRegistryEntryID(s, &d->id);
    registry_string(s, "Vendor Identification", d->vendor, sizeof d->vendor);
    registry_string(s, "Product Identification", d->product, sizeof d->product);
    registry_string(s, "Product Revision Level", d->revision, sizeof d->revision);
    d->type = registry_number(s, "Peripheral Device Type");
}

int cz_discover(cz_device *out, int max, cz_err *e)
{
    io_iterator_t iter;
    io_service_t s;
    int count = 0;
    kern_return_t kr = user_client_devices(&iter);

    if (kr != KERN_SUCCESS)
        return cz_fail(e, E_IO, "IOKit device search failed (0x%x)", kr);
    while (iter != 0 && (s = IOIteratorNext(iter)) != 0) {
        reg_device d;
        read_device(s, &d);
        if (is_supported(d.vendor, d.product) && count < max) {
            snprintf(out[count].address, sizeof out[count].address, "iokit:%llu", (unsigned long long)d.id);
            snprintf(out[count].vendor, sizeof out[count].vendor, "%s", d.vendor);
            snprintf(out[count].product, sizeof out[count].product, "%s", d.product);
            snprintf(out[count].revision, sizeof out[count].revision, "%s", d.revision);
            count++;
        }
        IOObjectRelease(s);
    }
    if (iter != 0)
        IOObjectRelease(iter);
    return count;
}

int cz_list_all_devices(FILE *f)
{
    io_iterator_t iter;
    io_service_t s;
    int count = 0;
    kern_return_t kr = user_client_devices(&iter);

    if (kr != KERN_SUCCESS) {
        fprintf(f, "IOKit device search failed (0x%x)\n", kr);
        return -1;
    }
    while (iter != 0 && (s = IOIteratorNext(iter)) != 0) {
        reg_device d;
        read_device(s, &d);
        fprintf(f, "iokit:%llu  %-8s %-16s %-4s type %d%s\n", (unsigned long long)d.id, d.vendor, d.product,
                d.revision, d.type, is_supported(d.vendor, d.product) ? "  (supported scanner)" : "");
        count++;
        IOObjectRelease(s);
    }
    if (iter != 0)
        IOObjectRelease(iter);
    if (count == 0)
        fprintf(f, "No SCSI devices without a driver were found. Check the SCSI card, its driver, the cable "
                   "and termination, and that the scanner was switched on before the Mac.\n");
    return count;
}

static int iokit_execute(cz_transport *t, const uint8_t *cdb, size_t cdb_len, int dir, uint8_t *buf, size_t len,
                         unsigned timeout_ms, cz_completion *c, cz_err *e)
{
    iokit_transport *it = (iokit_transport *)t;
    SCSITaskInterface **task;
    SCSITaskSGElement range;
    SCSI_Sense_Data sense;
    SCSITaskStatus status = kSCSITaskStatus_No_Status;
    UInt64 transferred = 0;
    UInt8 cdb_copy[16];
    UInt8 direction;
    IOReturn r;
    int result = -1;

    if (cdb_len > 16)
        return cz_fail(e, E_INVALID, "CDB longer than 16 bytes");
    memcpy(cdb_copy, cdb, cdb_len);
    direction = dir == DIR_IN ? kSCSIDataTransfer_FromTargetToInitiator
              : dir == DIR_OUT ? kSCSIDataTransfer_FromInitiatorToTarget
                               : kSCSIDataTransfer_NoDataTransfer;
    task = (*it->device)->CreateSCSITask(it->device);
    if (task == NULL)
        return cz_fail(e, E_IO, "cannot create a SCSI task");
    r = (*task)->SetCommandDescriptorBlock(task, cdb_copy, (UInt8)cdb_len);
    if (r != kIOReturnSuccess) {
        cz_fail(e, E_IO, "SetCommandDescriptorBlock failed (0x%x)", r);
        goto done;
    }
    memset(&range, 0, sizeof range);
    range.address = (uintptr_t)buf;
    range.length = len;
    r = (*task)->SetScatterGatherEntries(task, &range, len > 0 ? 1 : 0, len, direction);
    if (r != kIOReturnSuccess) {
        cz_fail(e, E_IO, "SetScatterGatherEntries failed (0x%x)", r);
        goto done;
    }
    (*task)->SetTimeoutDuration(task, timeout_ms);
    memset(&sense, 0, sizeof sense);
    r = (*task)->ExecuteTaskSync(task, &sense, &status, &transferred);
    if (r != kIOReturnSuccess) {
        cz_fail(e, E_IO, "SCSI task failed (0x%x)", r);
        goto done;
    }
    c->status = (uint8_t)status;
    c->transferred = transferred > len ? (uint32_t)len : (uint32_t)transferred;
    c->has_sense = 0;
    if (status != kSCSITaskStatus_GOOD && sense_ok((const uint8_t *)&sense, SENSE_LEN)) {
        memcpy(c->sense, &sense, SENSE_LEN);
        c->has_sense = 1;
    }
    result = 0;
done:
    (*task)->Release(task);
    return result;
}

static void iokit_close(cz_transport *t)
{
    iokit_transport *it = (iokit_transport *)t;
    (*it->device)->ReleaseExclusiveAccess(it->device);
    (*it->device)->Release(it->device);
    IODestroyPlugInInterface(it->plugin);
    free(it);
}

cz_transport *cz_open_local(const char *address, cz_err *e)
{
    io_iterator_t iter;
    io_service_t s, service = 0;
    IOCFPlugInInterface **plugin = NULL;
    SCSITaskDeviceInterface **device = NULL;
    iokit_transport *it;
    SInt32 score = 0;
    IOReturn kr;
    HRESULT hr;
    unsigned long long id = 0;
    char *end = NULL;

    if (strncmp(address, "iokit:", 6) == 0)
        id = strtoull(address + 6, &end, 10);
    if (id == 0 || end == NULL || *end != 0) {
        cz_fail(e, E_INVALID, "expected iokit:<registry id>, got %s", address);
        return NULL;
    }
    if (user_client_devices(&iter) == KERN_SUCCESS && iter != 0) {
        while (service == 0 && (s = IOIteratorNext(iter)) != 0) {
            uint64_t sid = 0;
            IORegistryEntryGetRegistryEntryID(s, &sid);
            if (sid == id)
                service = s;
            else
                IOObjectRelease(s);
        }
        IOObjectRelease(iter);
    }
    if (service == 0) {
        cz_fail(e, E_NOT_FOUND, "no IOKit device with id %llu, run cezanne-server --list again", id);
        return NULL;
    }
    kr = IOCreatePlugInInterfaceForService(service, kIOSCSITaskDeviceUserClientTypeID, kIOCFPlugInInterfaceID,
                                           &plugin, &score);
    IOObjectRelease(service);
    if (kr != kIOReturnSuccess || plugin == NULL) {
        cz_fail(e, E_IO, "cannot open the SCSI user client (0x%x)", kr);
        return NULL;
    }
    hr = (*plugin)->QueryInterface(plugin, CFUUIDGetUUIDBytes(kIOSCSITaskDeviceInterfaceID), (LPVOID *)&device);
    /* The plug in stays alive as long as the device. IODestroyPlugInInterface calls Stop, and
     * Stop closes the user client connection, after which CreateSCSITask returns NULL. */
    if (hr != S_OK || device == NULL) {
        IODestroyPlugInInterface(plugin);
        cz_fail(e, E_IO, "SCSITaskDeviceInterface not available (0x%x)", (int)hr);
        return NULL;
    }
    kr = (*device)->ObtainExclusiveAccess(device);
    if (kr != kIOReturnSuccess) {
        (*device)->Release(device);
        IODestroyPlugInInterface(plugin);
        cz_fail(e, E_IO,
                "the scanner is in use by another program (0x%x). Quit other scanning software and try again", kr);
        return NULL;
    }
    it = xcalloc(1, sizeof *it);
    it->plugin = plugin;
    it->device = device;
    it->base.execute = iokit_execute;
    it->base.close = iokit_close;
    snprintf(it->base.desc, sizeof it->base.desc, "IOKit %s", address);
    return &it->base;
}

#else /* not a Mac: the test build on another system reaches scanners only through a bridge */

int cz_discover(cz_device *out, int max, cz_err *e)
{
    (void)out;
    (void)max;
    return cz_fail(e, E_UNSUPPORTED, "this OS has no direct SCSI access, connect through cezanne-bridge");
}

cz_transport *cz_open_local(const char *address, cz_err *e)
{
    (void)address;
    cz_fail(e, E_UNSUPPORTED, "this OS has no direct SCSI access, connect through cezanne-bridge");
    return NULL;
}

int cz_list_all_devices(FILE *f)
{
    fprintf(f, "This build has no direct SCSI access. It is for Mac OS X.\n");
    return -1;
}
#endif
