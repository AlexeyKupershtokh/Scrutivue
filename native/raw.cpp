#include "libraw/libraw.h"
#include <cstdint>
#include <cstring>
#include <exception>
#include <limits>
#include <chrono>

// Own the LibRaw allocation until Rust finishes reading it. Each job has its own instance.
struct RawResult {
    LibRaw decoder;
    libraw_processed_image_t *image = nullptr;
    ~RawResult() { if (image) LibRaw::dcraw_clear_mem(image); }
};
struct RawView {
    void *owner;
    const unsigned char *data;
    uint32_t width, height, colors, bits, length, format, flip;
    // Sensor-coordinate rectangle BEFORE processing/orientation. Rust uses it
    // to apply DNG DefaultCrop relative to ActiveArea, not a second active crop.
    uint32_t raw_width, raw_height, left, top, area_width, area_height;
    uint64_t open_ns, unpack_ns, process_ns, bitmap_ns;
};

static int fail(char *error, size_t capacity, const char *message) {
    if (capacity) { std::strncpy(error, message, capacity - 1); error[capacity - 1] = 0; }
    return -1;
}

using CancelFn = int (*)(void *);
struct Cancellation { CancelFn callback; void *context; };
static int check_cancel(void *data, enum LibRaw_progress, int, int) {
    auto *cancel = static_cast<Cancellation *>(data);
    return cancel->callback ? cancel->callback(cancel->context) : 0;
}

static int raw_open_cancellable(const void *bytes, size_t length, int thumbnail, bool region,
                             RawView *out, char *error, size_t capacity,
                             CancelFn callback, void *context) {
    RawResult *result = nullptr;
    try {
        result = new RawResult();
        Cancellation cancel{callback, context};
        if (callback) result->decoder.set_progress_handler(check_cancel, &cancel);
        using Clock = std::chrono::steady_clock;
        auto start = Clock::now();
        auto elapsed = [&]() { return static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() - start).count()); };
        int status = result->decoder.open_buffer(const_cast<void *>(bytes), length);
        const auto open_ns = elapsed();
        uint64_t unpack_ns = 0, process_ns = 0, bitmap_ns = 0;
        if (status) { delete result; return fail(error, capacity, libraw_strerror(status)); }
        int memory_error = 0;
        auto area = result->decoder.imgdata.sizes;
        if (thumbnail) {
            const auto &list = result->decoder.imgdata.thumbs_list;
            int selected = -1;
            unsigned smallest = std::numeric_limits<unsigned>::max();
            for (int i = 0; i < list.thumbcount; ++i) {
                const auto &item = list.thumblist[i];
                if (item.tformat == LIBRAW_INTERNAL_THUMBNAIL_JPEG &&
                    item.twidth >= 160 && item.theight >= 112 && item.tlength < smallest) {
                    selected = i;
                    smallest = item.tlength;
                }
            }
            status = selected >= 0 ? result->decoder.unpack_thumb_ex(selected) : result->decoder.unpack_thumb();
            if (!status) result->image = result->decoder.dcraw_make_mem_thumb(&memory_error);
        } else {
            auto &params = result->decoder.imgdata.params;
            params.output_bps = 16;
            params.output_color = 1; // sRGB, consistent across all RAWs in the comparison.
            params.use_camera_wb = 1;
            params.no_auto_bright = 1;
            // A tile is not an independent exposure. no_auto_bright disables
            // histogram brightening, but NOT adjust_maximum's local white-point
            // normalization. Retain the DNG white level for all preview tiles.
            // Ordinary whole-frame development keeps its existing policy.
            if (region) params.adjust_maximum_thr = 0.f;
            params.user_qual = 0; // Fast bilinear development; no denoise or sharpening.
            params.med_passes = 0;
            start = Clock::now();
            status = result->decoder.unpack();
            unpack_ns = elapsed();
            area = result->decoder.imgdata.sizes;
            start = Clock::now();
            if (!status) status = result->decoder.dcraw_process();
            process_ns = elapsed();
            start = Clock::now();
            if (!status) result->image = result->decoder.dcraw_make_mem_image(&memory_error);
            bitmap_ns = elapsed();
        }
        if (status || !result->image || memory_error) {
            delete result;
            return fail(error, capacity, libraw_strerror(status ? status : memory_error));
        }
        const auto *image = result->image;
        *out = {result, image->data, image->width, image->height, image->colors,
                image->bits, image->data_size, static_cast<uint32_t>(image->type),
                static_cast<uint32_t>(result->decoder.imgdata.sizes.flip),
                area.raw_width, area.raw_height, area.left_margin, area.top_margin,
                area.width, area.height, open_ns, unpack_ns, process_ns, bitmap_ns};
        result->decoder.set_progress_handler(nullptr, nullptr);
        return 0;
    } catch (const std::exception &e) {
        delete result;
        return fail(error, capacity, e.what());
    } catch (...) {
        delete result;
        return fail(error, capacity, "LibRaw decoder exception");
    }
}
extern "C" int diff_raw_open_cancellable(const void *bytes, size_t length, int thumbnail,
                             RawView *out, char *error, size_t capacity,
                             CancelFn callback, void *context) {
    return raw_open_cancellable(bytes, length, thumbnail, false, out, error, capacity, callback, context);
}
extern "C" int diff_raw_open_region_cancellable(const void *bytes, size_t length,
                             RawView *out, char *error, size_t capacity,
                             CancelFn callback, void *context) {
    return raw_open_cancellable(bytes, length, 0, true, out, error, capacity, callback, context);
}
extern "C" int diff_raw_open(const void *bytes, size_t length, int thumbnail,
                             RawView *out, char *error, size_t capacity) {
    return diff_raw_open_cancellable(bytes, length, thumbnail, out, error, capacity, nullptr, nullptr);
}
extern "C" void diff_raw_close(void *owner) { delete static_cast<RawResult *>(owner); }
