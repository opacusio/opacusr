// NSCursor bridge — per-app cursor shapes for the macOS capture stack.
// The second ObjC shim next to sck_shim.m (compiled by build.rs on
// macOS targets only); the Rust side talks to it through this plain
// C API (src/cursor.rs, mod macos).
//
// What it does: reads [NSCursor currentSystemCursor] and renders the
// cursor's image at the caller's scale (device px per point — the
// streamed display's Retina scale) into a premultiplied RGBA8 bitmap,
// with the hotspot in flipped coordinates (top-left origin — NSCursor
// documents the cursor image as flipped, so it matches the wire
// format directly).
//
// Change detection: the serial bumps only when the RENDERED PIXELS
// (dims + hotspot + bytes, FNV-1a) actually change — same-cursor
// re-sets by apps (new NSCursor object, identical pixels) do NOT
// re-download the shape. Fast path: a repeat call with the same
// NSCursor object and scale skips rendering entirely.
//
// Threading: everything runs on our own serial queue. NSCursor reads
// and bitmap-context drawing do not touch the window server — safe off
// the main thread (the agent has no AppKit runloop to dispatch to).
// All AppKit bitmaps composite to PREMULTIPLIED alpha; the shim ships
// it as-is and the Rust side un-premultiplies (unit-tested there).

#import <AppKit/AppKit.h>
#import <dispatch/dispatch.h>
#import <math.h>
#import <stdint.h>
#import <stdlib.h>
#import <string.h>

// Return codes shared with src/cursor.rs (mod macos).
#define OPACUSR_CURSOR_OK 0
#define OPACUSR_CURSOR_UNAVAILABLE (-1)
#define OPACUSR_CURSOR_DST_TOO_SMALL (-2)

// Sanity cap: system cursors are 16-64 px (2x on Retina); a hostile or
// buggy custom cursor must not make us allocate unbounded memory.
#define OPACUSR_CURSOR_MAX_DIM 256

static dispatch_queue_t g_queue = NULL;

// Cursor state — every access is dispatch_sync'd onto g_queue.
static NSCursor *g_cur = nil; // last seen NSCursor object (fast path)
static uint64_t g_serial = 0; // 0 = never seen a cursor
static uint64_t g_hash = 0;
static uint32_t g_w = 0, g_h = 0, g_hx = 0, g_hy = 0;
static uint32_t g_scale100 = 0; // scale * 100 of the cached render
static uint8_t *g_px = NULL;    // premultiplied RGBA, packed rows
static size_t g_cap = 0;

static void cursor_init_queue(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        g_queue = dispatch_queue_create("io.opacusr.cursor", DISPATCH_QUEUE_SERIAL);
    });
}

// FNV-1a over the identity of the rendered shape: dims + hotspot + the
// premultiplied bytes. Equal hashes mean "same bitmap the viewer sees".
static uint64_t cursor_hash(uint32_t w, uint32_t h, uint32_t hx, uint32_t hy,
                            const uint8_t *px, size_t n) {
    uint64_t hash = 1469598103934665603ULL;
    uint32_t dims[4] = {w, h, hx, hy};
    const uint8_t *d = (const uint8_t *)dims;
    for (size_t i = 0; i < sizeof(dims); i++) {
        hash ^= d[i];
        hash *= 1099511628211ULL;
    }
    for (size_t i = 0; i < n; i++) {
        hash ^= px[i];
        hash *= 1099511628211ULL;
    }
    return hash;
}

static uint32_t cursor_clamp_dim(float v) {
    if (!(v > 0.0f)) return 1;
    if (v > OPACUSR_CURSOR_MAX_DIM) return OPACUSR_CURSOR_MAX_DIM;
    return (uint32_t)lroundf(v);
}

// On g_queue: re-render when the cursor object or the requested scale
// changed. Returns OPACUSR_CURSOR_OK when g_* describe the current
// cursor, OPACUSR_CURSOR_UNAVAILABLE otherwise (state untouched).
static int cursor_refresh(float scale) {
    NSCursor *cur = [NSCursor currentSystemCursor];
    if (cur == nil || cur.image == nil) return OPACUSR_CURSOR_UNAVAILABLE;
    CGFloat pw = cur.image.size.width, ph = cur.image.size.height;
    if (!(pw > 0.0) || !(ph > 0.0)) return OPACUSR_CURSOR_UNAVAILABLE;

    uint32_t scale100 = (uint32_t)lroundf(scale * 100.0f);
    if (scale100 == 0) scale100 = 100;
    // Fast path: same cursor object at the same scale — the cached
    // render IS the current shape (poll() calls this every frame).
    if (cur == g_cur && scale100 == g_scale100) return OPACUSR_CURSOR_OK;

    float clamped = scale;
    if (!(clamped > 0.0f)) clamped = 1.0f;
    if (clamped > 4.0f) clamped = 4.0f;
    uint32_t tw = cursor_clamp_dim(pw * clamped);
    uint32_t th = cursor_clamp_dim(ph * clamped);

    NSBitmapImageRep *rep = [[NSBitmapImageRep alloc]
        initWithBitmapDataPlanes:NULL
        pixelsWide:(NSInteger)tw
        pixelsHigh:(NSInteger)th
        bitsPerSample:8
        samplesPerPixel:4
        hasAlpha:YES
        isPlanar:NO
        colorSpaceName:NSCalibratedRGBColorSpace
        bytesPerRow:0
        bitsPerPixel:0];
    NSGraphicsContext *ctx =
        rep != nil ? [NSGraphicsContext graphicsContextWithBitmapImageRep:rep] : nil;
    if (rep == nil || ctx == nil) return OPACUSR_CURSOR_UNAVAILABLE;

    [NSGraphicsContext saveGraphicsState];
    [NSGraphicsContext setCurrentContext:ctx];
    [cur.image drawInRect:NSMakeRect(0, 0, tw, th)
                  fromRect:NSZeroRect
                operation:NSCompositingOperationSourceOver
                 fraction:1.0
           respectFlipped:YES
                    hints:nil];
    [NSGraphicsContext restoreGraphicsState];

    uint8_t *src = (uint8_t *)[rep bitmapData];
    if (src == NULL) return OPACUSR_CURSOR_UNAVAILABLE;
    size_t bpr = (size_t)[rep bytesPerRow];
    size_t row = (size_t)tw * 4;
    size_t need = row * th;

    // Compact the render into our own packed buffer, then hash it.
    if (g_cap < need) {
        uint8_t *nb = (uint8_t *)realloc(g_px, need);
        if (nb == NULL) return OPACUSR_CURSOR_UNAVAILABLE;
        g_px = nb;
        g_cap = need;
    }
    for (uint32_t y = 0; y < th; y++) {
        memcpy(g_px + (size_t)y * row, src + (size_t)y * bpr, row);
    }

    NSPoint hot = cur.hotSpot; // flipped: top-left origin, in POINTS
    uint32_t hx = cursor_clamp_dim(hot.x * clamped);
    uint32_t hy = cursor_clamp_dim(hot.y * clamped);
    if (hx >= tw) hx = tw - 1;
    if (hy >= th) hy = th - 1;

    uint64_t hash = cursor_hash(tw, th, hx, hy, g_px, need);
    if (g_serial != 0 && hash == g_hash) {
        // Same pixels (an app re-set an identical cursor): the viewer
        // must NOT re-download — keep the serial, just track the object.
        g_cur = cur;
        g_scale100 = scale100;
        return OPACUSR_CURSOR_OK;
    }

    g_hash = hash;
    g_serial = g_serial == 0 ? 1 : g_serial + 1;
    g_cur = cur;
    g_scale100 = scale100;
    g_w = tw;
    g_h = th;
    g_hx = hx;
    g_hy = hy;
    return OPACUSR_CURSOR_OK;
}

uint64_t opacusr_cursor_serial(float scale) {
    cursor_init_queue();
    __block uint64_t serial = 0;
    dispatch_sync(g_queue, ^{
        // Renders/hashes the current cursor when the object or the
        // scale drifted (serial bumps on real content change — dims and
        // hotspot are part of the hash). A nil cursor keeps the last
        // known serial (0 = never seen → the Rust side falls back to
        // its arrow sprite).
        if (cursor_refresh(scale) == OPACUSR_CURSOR_OK || g_serial != 0) {
            serial = g_serial;
        }
    });
    return serial;
}

int opacusr_cursor_image(float scale, uint8_t *dst, uint32_t cap, uint32_t *w,
                         uint32_t *h, uint32_t *hot_x, uint32_t *hot_y,
                         uint64_t *serial) {
    cursor_init_queue();
    if (w == NULL || h == NULL || hot_x == NULL || hot_y == NULL || serial == NULL) {
        return OPACUSR_CURSOR_UNAVAILABLE;
    }
    __block int rc = OPACUSR_CURSOR_OK;
    dispatch_sync(g_queue, ^{
        if (cursor_refresh(scale) != OPACUSR_CURSOR_OK) {
            rc = OPACUSR_CURSOR_UNAVAILABLE;
            return;
        }
        *w = g_w;
        *h = g_h;
        *hot_x = g_hx;
        *hot_y = g_hy;
        *serial = g_serial;
        size_t need = (size_t)g_w * g_h * 4;
        if (dst == NULL || cap < need) {
            rc = OPACUSR_CURSOR_DST_TOO_SMALL;
            return;
        }
        memcpy(dst, g_px, need);
    });
    return rc;
}
