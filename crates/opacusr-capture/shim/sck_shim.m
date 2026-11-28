// ScreenCaptureKit bridge — the ONLY Objective-C in the macOS capture
// stack. Everything the Rust side touches is this plain C API; the
// ObjC glue below is small, auditable, and compiled with ARC so the
// retain/release story is the compiler's, not ours.
//
// Design (see src/macos_sck.rs for the Rust wrapper):
// * one stream per handle (the handle IS the output object, retained
//   for the caller via __bridge_retained and released in sck_stop —
//   so a dropped SckStream can never tear down a freshly started one);
// * the stream delivers frames on our own serial queue; the callback
//   compacts CVPixelBuffer rows (bytesPerRow padding) into a packed
//   BGRA buffer under a pthread_mutex — poll() copies out of that;
// * showsCursor = NO: the CG fallback does not capture the pointer
//   either, the viewer draws the cursor from the overlay metadata;
// * wrong pixel format (SCK ignoring pixelFormat on some versions) or
//   a delegate stop error surfaces as a poll() return code, and the
//   Rust backend falls back to CoreGraphics snapshots.
//
// All the SCStream entry points live behind @available(macOS 12.3, *)
// — the same OS that deprecated CGDisplayCreateImage (macOS 15) can no
// longer regress this file.

#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <objc/runtime.h>
#import <pthread.h>
#import <stdint.h>
#import <stdio.h>
#import <stdlib.h>
#import <string.h>

// Return codes shared with src/macos_sck.rs.
#define OPACUSR_SCK_OK 0
#define OPACUSR_SCK_WRONG_FORMAT 1
#define OPACUSR_SCK_UNSUPPORTED (-1)
#define OPACUSR_SCK_ERR (-2)
#define OPACUSR_SCK_NOT_RUNNING (-3)
#define OPACUSR_SCK_NO_FRAME (-4)
#define OPACUSR_SCK_STOPPED (-5)

// Hard sanity cap on frame geometry (a hostile/buggy callback must not
// make us allocate unbounded memory).
#define OPACUSR_SCK_MAX_DIM 16384

// Shim timeouts (seconds): shareable-content query, stream start, and
// the first-frame wait. A stream that never delivers a first frame is
// reported as an error so the Rust backend falls back to CoreGraphics.
#define OPACUSR_SCK_TIMEOUT_SEC 5

typedef struct OpacusRSckFrameState {
    pthread_mutex_t lock;
    // Packed BGRA, `w * 4` bytes per row (stride compacted).
    uint8_t *buf;
    size_t cap;
    uint32_t w, h;
    uint64_t seq;
    // Set in the callback; read under the same lock in poll().
    int wrong_format;
    int stopped;
    char stop_msg[320];
} OpacusRSckFrameState;

@interface OpacusRSckOutput : NSObject <SCStreamOutput, SCStreamDelegate>
{
  @public
    struct OpacusRSckFrameState st;
    // Strong under ARC while the stream is live.
    SCStream *stream;
    dispatch_queue_t queue;
    dispatch_semaphore_t first_sem;
}
@end

@implementation OpacusRSckOutput

- (instancetype)init {
    self = [super init];
    if (self == nil) return self;
    memset(&self->st, 0, sizeof(self->st));
    if (pthread_mutex_init(&self->st.lock, NULL) != 0) {
        return nil;
    }
    self->queue = dispatch_queue_create("io.opacusr.sck-frames", DISPATCH_QUEUE_SERIAL);
    return self;
}

- (void)dealloc {
    free(self->st.buf);
    pthread_mutex_destroy(&self->st.lock);
}

// SCStreamOutput — one delivered display frame. The SDK's output type
// is SCStreamOutputType (the protocol was NOT declared with SCFrameType
// — caught by the macOS CI compile).
- (void)stream:(SCStream *)scStream
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
                   ofType:(SCStreamOutputType)type {
    (void)scStream;
    if (type != SCStreamOutputTypeScreen) return;
    CVPixelBufferRef pb = (CVPixelBufferRef)CMSampleBufferGetImageBuffer(sampleBuffer);
    if (pb == NULL) return;
    if (CVPixelBufferGetPixelFormatType(pb) != kCVPixelFormatType_32BGRA) {
        pthread_mutex_lock(&self->st.lock);
        self->st.wrong_format = 1;
        pthread_mutex_unlock(&self->st.lock);
        return;
    }
    size_t w = CVPixelBufferGetWidth(pb);
    size_t h = CVPixelBufferGetHeight(pb);
    size_t bpr = CVPixelBufferGetBytesPerRow(pb);
    if (w == 0 || h == 0 || w > OPACUSR_SCK_MAX_DIM || h > OPACUSR_SCK_MAX_DIM) return;
    size_t want = w * h * 4;

    CVPixelBufferLockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
    const uint8_t *base = (const uint8_t *)CVPixelBufferGetBaseAddress(pb);
    if (base != NULL) {
        pthread_mutex_lock(&self->st.lock);
        if (self->st.cap < want) {
            uint8_t *nb = (uint8_t *)realloc(self->st.buf, want);
            if (nb != NULL) {
                self->st.buf = nb;
                self->st.cap = want;
            }
        }
        if (self->st.cap >= want) {
            // Compact rows: bytesPerRow may carry padding the CG path
            // never sees (CGDataProviderCopyData is already packed).
            for (size_t y = 0; y < h; y++) {
                memcpy(self->st.buf + y * w * 4, base + y * bpr, w * 4);
            }
            self->st.w = (uint32_t)w;
            self->st.h = (uint32_t)h;
            self->st.seq++;
        }
        pthread_mutex_unlock(&self->st.lock);
    }
    CVPixelBufferUnlockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
    if (self->first_sem != NULL) {
        dispatch_semaphore_signal(self->first_sem);
    }
}

// SCStreamDelegate — fatal stream error (e.g. permission revoked
// mid-session). The stream keeps delivering nothing after this; poll()
// reports OPACUSR_SCK_STOPPED with the reason.
- (void)stream:(SCStream *)scStream didStopWithError:(NSError *)error {
    (void)scStream;
    pthread_mutex_lock(&self->st.lock);
    self->st.stopped = 1;
    snprintf(self->st.stop_msg, sizeof(self->st.stop_msg),
             "SCStream stopped: %s (%ld)",
             error.localizedDescription.UTF8String ?: "unknown error",
             (long)error.code);
    pthread_mutex_unlock(&self->st.lock);
}

@end

static void sck_set_err(char *errbuf, uint32_t cap, NSString *msg) {
    if (errbuf == NULL || cap == 0) return;
    const char *utf8 = msg.UTF8String;
    if (utf8 == NULL) utf8 = "unknown ScreenCaptureKit error";
    size_t n = strlen(utf8);
    if (n > (size_t)cap - 1) n = (size_t)cap - 1;
    memcpy(errbuf, utf8, n);
    errbuf[n] = 0;
}

static void sck_err_ns(char *errbuf, uint32_t cap, NSError *err) {
    if (err != NULL) {
        sck_set_err(errbuf, cap,
                    [NSString stringWithFormat:@"%@ (%ld; %s)",
                        err.localizedDescription ?: @"ScreenCaptureKit error",
                        (long)err.code, err.domain.UTF8String ?: ""]);
    } else {
        sck_set_err(errbuf, cap, @"ScreenCaptureKit error");
    }
}

int opacusr_sck_supported(void) {
    if (@available(macOS 12.3, *)) {
        return objc_getClass("SCStream") != NULL ? 1 : 0;
    }
    return 0;
}

// Returns a nonzero handle on success, 0 on failure (errbuf filled).
uintptr_t opacusr_sck_start(uint32_t display_id, uint32_t width, uint32_t height,
                            uint32_t fps, char *errbuf, uint32_t errbuf_cap) {
    if (@available(macOS 12.3, *)) {
        if (objc_getClass("SCStream") == NULL) {
            sck_set_err(errbuf, errbuf_cap, @"ScreenCaptureKit unavailable");
            return 0;
        }
        if (width == 0 || height == 0 || fps == 0) {
            sck_set_err(errbuf, errbuf_cap, @"bad stream parameters");
            return 0;
        }

        OpacusRSckOutput *out = [[OpacusRSckOutput alloc] init];
        if (out == nil) {
            sck_set_err(errbuf, errbuf_cap, @"output object allocation failed");
            return 0;
        }
        if (out->queue == NULL) {
            sck_set_err(errbuf, errbuf_cap, @"dispatch queue allocation failed");
            return 0;
        }

        // SCShareableContent: the SCK catalog of what may be captured.
        // Needs the Screen Recording TCC permission — without it this
        // errors (and the CG fallback will surface the user-facing
        // hint). Runs on our own queue; querying from an arbitrary
        // thread is fine, blocking the main queue is not.
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);
        __block SCShareableContent *content = nil;
        __block NSError *content_err = nil;
        dispatch_async(out->queue, ^{
            [SCShareableContent
                getShareableContentExcludingDesktopWindows:NO
                                       onScreenWindowsOnly:YES
                                         completionHandler:^(SCShareableContent *c, NSError *e) {
                    content = c;
                    content_err = e;
                    dispatch_semaphore_signal(sem);
                }];
        });
        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW,
                OPACUSR_SCK_TIMEOUT_SEC * NSEC_PER_SEC));
        if (content == nil) {
            sck_err_ns(errbuf, errbuf_cap, content_err);
            return 0;
        }

        SCDisplay *target = nil;
        for (SCDisplay *d in content.displays) {
            if (d.displayID == (CGDirectDisplayID)display_id) {
                target = d;
                break;
            }
        }
        if (target == nil) {
            sck_set_err(errbuf, errbuf_cap,
                        [NSString stringWithFormat:@"display %u not shareable (%lu known)",
                            display_id, (unsigned long)content.displays.count]);
            return 0;
        }

        // Whole display, nothing excluded (same view as the CG path).
        SCContentFilter *filter =
            [[SCContentFilter alloc] initWithDisplay:target excludingWindows:@[]];
        SCStreamConfiguration *cfg = [[SCStreamConfiguration alloc] init];
        cfg.width = (NSUInteger)width;
        cfg.height = (NSUInteger)height;
        cfg.showsCursor = NO;
        cfg.queueDepth = 3;
        cfg.minimumFrameInterval = CMTimeMake(1, (int32_t)fps);
        cfg.pixelFormat = kCVPixelFormatType_32BGRA;

        SCStream *stream = [[SCStream alloc] initWithFilter:filter
                                              configuration:cfg
                                                   delegate:out];
        if (stream == nil) {
            sck_set_err(errbuf, errbuf_cap, @"SCStream allocation failed");
            return 0;
        }
        out->stream = stream;

        NSError *add_err = nil;
        if (![stream addStreamOutput:out
                                type:SCStreamOutputTypeScreen
                   sampleHandlerQueue:out->queue
                               error:&add_err]) {
            sck_err_ns(errbuf, errbuf_cap, add_err);
            return 0;
        }

        __block NSError *start_err = nil;
        dispatch_semaphore_t start_sem = dispatch_semaphore_create(0);
        [stream startCaptureWithCompletionHandler:^(NSError *e) {
            start_err = e;
            dispatch_semaphore_signal(start_sem);
        }];
        dispatch_semaphore_wait(start_sem, dispatch_time(DISPATCH_TIME_NOW,
                OPACUSR_SCK_TIMEOUT_SEC * NSEC_PER_SEC));
        if (start_err != nil) {
            sck_err_ns(errbuf, errbuf_cap, start_err);
            return 0;
        }

        // The first frame must arrive before start() returns: capture()
        // may be pulled the instant the session starts, and a stream
        // that never delivers (broken permission state, dead display)
        // is an error, not a black stream.
        out->first_sem = dispatch_semaphore_create(0);
        long got = dispatch_semaphore_wait(out->first_sem,
                dispatch_time(DISPATCH_TIME_NOW,
                        OPACUSR_SCK_TIMEOUT_SEC * NSEC_PER_SEC));
        out->first_sem = NULL;
        if (got != 0) {
            [stream stopCaptureWithCompletionHandler:^(NSError *e) { (void)e; }];
            sck_set_err(errbuf, errbuf_cap, @"no first frame from the stream");
            return 0;
        }

        // Hand ownership to the caller (+1, released in sck_stop).
        return (uintptr_t)(__bridge_retained CFTypeRef)out;
    }
    sck_set_err(errbuf, errbuf_cap, @"ScreenCaptureKit needs macOS 12.3+");
    return 0;
}

int opacusr_sck_poll(uintptr_t handle, uint8_t *dst, uint32_t dst_cap,
                     uint32_t *w, uint32_t *h, uint64_t *seq) {
    if (handle == 0 || w == NULL || h == NULL || seq == NULL) {
        return OPACUSR_SCK_NOT_RUNNING;
    }
    OpacusRSckOutput *out = (__bridge OpacusRSckOutput *)(void *)handle;
    pthread_mutex_lock(&out->st.lock);
    if (out->st.wrong_format) {
        pthread_mutex_unlock(&out->st.lock);
        return OPACUSR_SCK_WRONG_FORMAT;
    }
    if (out->st.stopped) {
        pthread_mutex_unlock(&out->st.lock);
        return OPACUSR_SCK_STOPPED;
    }
    // Geometry is reported even when dst cannot hold the frame — the
    // Rust side resizes and retries once.
    *w = out->st.w;
    *h = out->st.h;
    *seq = out->st.seq;
    if (out->st.seq == 0 || out->st.buf == NULL) {
        pthread_mutex_unlock(&out->st.lock);
        return OPACUSR_SCK_NO_FRAME;
    }
    size_t need = (size_t)out->st.w * out->st.h * 4;
    if (dst == NULL || dst_cap < need) {
        pthread_mutex_unlock(&out->st.lock);
        return OPACUSR_SCK_ERR;
    }
    memcpy(dst, out->st.buf, need);
    pthread_mutex_unlock(&out->st.lock);
    return OPACUSR_SCK_OK;
}

int opacusr_sck_stopmsg(uintptr_t handle, char *buf, uint32_t cap) {
    if (handle == 0 || buf == NULL || cap == 0) {
        return OPACUSR_SCK_NOT_RUNNING;
    }
    OpacusRSckOutput *out = (__bridge OpacusRSckOutput *)(void *)handle;
    pthread_mutex_lock(&out->st.lock);
    size_t n = strlen(out->st.stop_msg);
    if (n > (size_t)cap - 1) n = (size_t)cap - 1;
    memcpy(buf, out->st.stop_msg, n);
    buf[n] = 0;
    pthread_mutex_unlock(&out->st.lock);
    return OPACUSR_SCK_OK;
}

// Tears the stream down and releases the ownership taken in start().
// Safe to call with a stale handle (no-op unless it is the live one —
// handles are object pointers, so a match IS the same stream).
void opacusr_sck_stop(uintptr_t handle) {
    if (handle == 0) return;
    OpacusRSckOutput *out = (__bridge_transfer OpacusRSckOutput *)(void *)handle;
    SCStream *stream = out->stream;
    if (stream != nil) {
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);
        [stream stopCaptureWithCompletionHandler:^(NSError *e) {
            (void)e;
            dispatch_semaphore_signal(sem);
        }];
        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW,
                OPACUSR_SCK_TIMEOUT_SEC * NSEC_PER_SEC));
        [stream removeStreamOutput:out type:SCStreamFrameOutputTypeScreen];
        out->stream = nil;
        out->queue = nil;
    }
    // `out` is released here (__bridge_transfer hands the +1 to ARC).
}
