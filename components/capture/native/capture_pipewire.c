/* Portal-only PipeWire reader. Each instance owns one restricted remote FD.
 * Packed RGB frames are copied into a single bounded latest-frame slot; no
 * desktop socket discovery, device access, or capture permission fallback. */
#include <pipewire/pipewire.h>
#include <pthread.h>
#include <stdatomic.h>
#include <spa/param/video/format-utils.h>
#include <spa/param/buffers.h>
#include <spa/buffer/meta.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

struct capture_info {
    uint32_t width, height, size, depth;
    double received;
    char pixel_format[24], transfer[24], primaries[24];
};
struct capture {
    struct pw_thread_loop *loop;
    struct pw_context *context;
    struct pw_core *core;
    struct pw_stream *stream;
    struct spa_hook core_listener, stream_listener;
    pthread_mutex_t mutex;
    struct capture_info info;
    uint8_t *pixels;
    size_t capacity;
    struct spa_video_info_raw format;
    char error[256];
    atomic_bool has_frame, error_set;
};
static void fail(struct capture *c, const char *message) {
    snprintf(c->error, sizeof(c->error), "%s", message ? message : "PipeWire capture failed");
    atomic_store(&c->error_set, true);
}
static void core_error(void *data, uint32_t id, int seq, int res, const char *message) {
    (void)seq;
    struct capture *c = data;
    pthread_mutex_lock(&c->mutex);
    if (id == PW_ID_CORE && res < 0) fail(c, message);
    pthread_mutex_unlock(&c->mutex);
}
static void state_changed(void *data, enum pw_stream_state old, enum pw_stream_state state, const char *error) {
    struct capture *c = data;
    pthread_mutex_lock(&c->mutex);
    if (state == PW_STREAM_STATE_ERROR) fail(data, error);
    if (state == PW_STREAM_STATE_UNCONNECTED && old != PW_STREAM_STATE_UNCONNECTED)
        fail(data, "The portal stream was closed");
    pthread_mutex_unlock(&c->mutex);
}
static void format_changed(void *data, uint32_t id, const struct spa_pod *param) {
    struct capture *c = data;
    if (id != SPA_PARAM_Format || !param) return;
    pthread_mutex_lock(&c->mutex);
    if (spa_format_video_raw_parse(param, &c->format) < 0) {
        fail(c, "Unsupported portal video format"); goto finish;
    }
    const char *pixel = NULL;
    c->info.depth = 8;
    switch (c->format.format) {
    case SPA_VIDEO_FORMAT_BGRx: pixel = "bgr0"; break;
    case SPA_VIDEO_FORMAT_RGBx: pixel = "rgb0"; break;
    case SPA_VIDEO_FORMAT_BGRA: pixel = "bgra"; break;
    case SPA_VIDEO_FORMAT_RGBA: pixel = "rgba"; break;
    case SPA_VIDEO_FORMAT_xRGB: pixel = "0rgb"; break;
    case SPA_VIDEO_FORMAT_ARGB: pixel = "argb"; break;
    case SPA_VIDEO_FORMAT_xBGR: pixel = "0bgr"; break;
    case SPA_VIDEO_FORMAT_ABGR: pixel = "abgr"; break;
    case SPA_VIDEO_FORMAT_ABGR_210LE:
    case SPA_VIDEO_FORMAT_xBGR_210LE: pixel = "x2bgr10le"; c->info.depth = 10; break;
    case SPA_VIDEO_FORMAT_ARGB_210LE:
    case SPA_VIDEO_FORMAT_xRGB_210LE: pixel = "x2rgb10le"; c->info.depth = 10; break;
    default: fail(c, "The portal did not provide a supported packed RGB format"); goto finish;
    }
    uint32_t w = c->format.size.width, h = c->format.size.height;
    if (!w || !h || w > 32768 || h > 32768 || (uint64_t)w*h > 100000000) {
        fail(c, "Unsupported portal image size"); goto finish;
    }
    size_t size = (size_t)w*h*4;
    if (size > c->capacity) {
        void *pixels = realloc(c->pixels, size);
        if (!pixels) { fail(c, "Not enough memory for the capture frame"); goto finish; }
        c->pixels = pixels; c->capacity = size;
    }
    atomic_store(&c->has_frame, false);
    c->info.width = w; c->info.height = h; c->info.size = 0;
    snprintf(c->info.pixel_format, sizeof(c->info.pixel_format), "%s", pixel);
    const char *transfer = "iec61966-2-1", *primaries = "bt709";
    switch (c->format.transfer_function) {
    case SPA_VIDEO_TRANSFER_SMPTE2084: transfer = "smpte2084"; break;
    case SPA_VIDEO_TRANSFER_ARIB_STD_B67: transfer = "arib-std-b67"; break;
    case SPA_VIDEO_TRANSFER_BT709: transfer = "bt709"; break;
    case SPA_VIDEO_TRANSFER_GAMMA22: transfer = "bt470m"; break;
    case SPA_VIDEO_TRANSFER_GAMMA10: transfer = "linear"; break;
    default: break;
    }
    if (c->format.color_primaries == SPA_VIDEO_COLOR_PRIMARIES_BT2020) primaries = "bt2020";
    if (c->format.color_primaries == SPA_VIDEO_COLOR_PRIMARIES_SMPTEEG432) primaries = "smpte432";
    snprintf(c->info.transfer, sizeof(c->info.transfer), "%s", transfer);
    snprintf(c->info.primaries, sizeof(c->info.primaries), "%s", primaries);
    uint8_t storage[1024];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(storage, sizeof(storage));
    const struct spa_pod *params[] = {
        spa_pod_builder_add_object(&b, SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
            SPA_PARAM_BUFFERS_buffers, SPA_POD_CHOICE_RANGE_Int(4, 2, 8),
            SPA_PARAM_BUFFERS_blocks, SPA_POD_Int(1),
            SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int(1 << SPA_DATA_MemFd)),
        spa_pod_builder_add_object(&b, SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta,
            SPA_PARAM_META_type, SPA_POD_Id(SPA_META_Header),
            SPA_PARAM_META_size, SPA_POD_Int(sizeof(struct spa_meta_header)))
    };
    pw_stream_update_params(c->stream, params, 2);
finish:
    pthread_mutex_unlock(&c->mutex);
}
static void process(void *data) {
    struct capture *c = data;
    struct pw_buffer *pw, *latest = NULL;
    while ((pw = pw_stream_dequeue_buffer(c->stream))) {
        if (latest) pw_stream_queue_buffer(c->stream, latest);
        latest = pw;
    }
    if (!latest) return;
    /* Never wait on a reader or allocate memory on PipeWire's data thread. */
    if (pthread_mutex_trylock(&c->mutex) != 0) {
        pw_stream_queue_buffer(c->stream, latest); return;
    }
    struct spa_buffer *b = latest->buffer;
    struct spa_meta_header *header = spa_buffer_find_meta_data(b, SPA_META_Header, sizeof(*header));
    if (!b->n_datas || (header && (header->flags & SPA_META_HEADER_FLAG_CORRUPTED))) goto done;
    struct spa_data *d = &b->datas[0];
    uint32_t row = c->info.width * 4, h = c->info.height;
    if (!d->data || !d->chunk || !d->chunk->size || d->chunk->flags & SPA_CHUNK_FLAG_CORRUPTED) goto done;
    int32_t stride = d->chunk->stride;
    if (!row || !h || stride < (int32_t)row || (uint64_t)d->chunk->offset + (uint64_t)(h-1)*stride + row > d->maxsize) {
        fail(c, "The portal returned an invalid image buffer"); goto done;
    }
    size_t size = (size_t)row*h;
    if (size > c->capacity) goto done;
    for (uint32_t y = 0; y < h; y++)
        memcpy(c->pixels + (size_t)y*row, (uint8_t*)d->data + d->chunk->offset + (size_t)y*stride, row);
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    c->info.received = now.tv_sec + now.tv_nsec / 1e9;
    /* Portal frame PTS is monotonic: do not relabel a buffered pre-lock image. */
    if (header && header->pts > 0 && header->pts / 1e9 < c->info.received)
        c->info.received = header->pts / 1e9;
    c->info.size = size;
    atomic_store(&c->has_frame, true);
done:
    pthread_mutex_unlock(&c->mutex);
    pw_stream_queue_buffer(c->stream, latest);
}
static const struct pw_core_events core_events = { PW_VERSION_CORE_EVENTS, .error = core_error };
static const struct pw_stream_events stream_events = {
    PW_VERSION_STREAM_EVENTS, .state_changed = state_changed,
    .param_changed = format_changed, .process = process
};
void capture_close(struct capture *c) {
    if (!c) return;
    if (c->loop) pw_thread_loop_stop(c->loop);
    if (c->stream) pw_stream_destroy(c->stream);
    if (c->core) pw_core_disconnect(c->core);
    if (c->context) pw_context_destroy(c->context);
    if (c->loop) pw_thread_loop_destroy(c->loop);
    pthread_mutex_destroy(&c->mutex);
    free(c->pixels); free(c);
}
struct capture *capture_open(int fd, uint32_t node, uint32_t fps) {
    pw_init(NULL, NULL);
    struct capture *c = calloc(1, sizeof(*c));
    if (!c) return NULL;
    pthread_mutex_init(&c->mutex, NULL);
    c->loop = pw_thread_loop_new("portal-capture", NULL);
    if (!c->loop) goto error;
    c->context = pw_context_new(pw_thread_loop_get_loop(c->loop), NULL, 0);
    if (!c->context) goto error;
    int remote = dup(fd);
    if (remote < 0) goto error;
    c->core = pw_context_connect_fd(c->context, remote, NULL, 0);
    if (!c->core) goto error;
    pw_core_add_listener(c->core, &c->core_listener, &core_events, c);
    c->stream = pw_stream_new(c->core, "Desktop Capture", pw_properties_new(
        PW_KEY_MEDIA_TYPE, "Video", PW_KEY_MEDIA_CATEGORY, "Capture", PW_KEY_MEDIA_ROLE, "Screen", NULL));
    if (!c->stream) goto error;
    pw_stream_add_listener(c->stream, &c->stream_listener, &stream_events, c);
    uint8_t storage[2048];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(storage, sizeof(storage));
    const struct spa_pod *format = spa_pod_builder_add_object(&b,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format, SPA_POD_CHOICE_ENUM_Id(13,
            SPA_VIDEO_FORMAT_xBGR_210LE, SPA_VIDEO_FORMAT_xBGR_210LE, SPA_VIDEO_FORMAT_xRGB_210LE, SPA_VIDEO_FORMAT_ABGR_210LE, SPA_VIDEO_FORMAT_ARGB_210LE,
            SPA_VIDEO_FORMAT_BGRx, SPA_VIDEO_FORMAT_RGBx, SPA_VIDEO_FORMAT_BGRA, SPA_VIDEO_FORMAT_RGBA,
            SPA_VIDEO_FORMAT_xRGB, SPA_VIDEO_FORMAT_ARGB, SPA_VIDEO_FORMAT_xBGR, SPA_VIDEO_FORMAT_ABGR),
        SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(
            &SPA_RECTANGLE(1920,1080), &SPA_RECTANGLE(1,1), &SPA_RECTANGLE(32768,32768)),
        SPA_FORMAT_VIDEO_framerate, SPA_POD_Fraction(&SPA_FRACTION(0,1)),
        SPA_FORMAT_VIDEO_maxFramerate, SPA_POD_CHOICE_RANGE_Fraction(
            &SPA_FRACTION(fps,1), &SPA_FRACTION(1,1), &SPA_FRACTION(fps,1)));
    /* Consume in the processing callback: deferring to a main-loop callback
     * can miss the initial buffer of a damage-driven static source. */
    if (pw_stream_connect(c->stream, PW_DIRECTION_INPUT, node,
            PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS | PW_STREAM_FLAG_DONT_RECONNECT | PW_STREAM_FLAG_RT_PROCESS,
            &format, 1) < 0) goto error;
    if (pw_thread_loop_start(c->loop) < 0) goto error;
    return c;
error:
    capture_close(c); return NULL;
}
/* Copy under the PipeWire loop lock, allowing Python to own the returned data. */
int capture_read(struct capture *c, struct capture_info *info, void **pixels, char *error, size_t error_size) {
    int status = 0;
    /* Do not contend with delivery of a static source's only first frame. */
    if (!atomic_load(&c->has_frame) && !atomic_load(&c->error_set)) return 0;
    pw_thread_loop_lock(c->loop);
    pthread_mutex_lock(&c->mutex);
    if (c->error[0]) { snprintf(error, error_size, "%s", c->error); status = -1; }
    else if (c->info.size) {
        *pixels = malloc(c->info.size);
        if (!*pixels) { snprintf(error, error_size, "Not enough memory for the capture frame"); status = -1; }
        else { *info = c->info; memcpy(*pixels, c->pixels, info->size); status = 1; }
    }
    pthread_mutex_unlock(&c->mutex);
    pw_thread_loop_unlock(c->loop);
    return status;
}
void capture_release(void *pixels) { free(pixels); }
