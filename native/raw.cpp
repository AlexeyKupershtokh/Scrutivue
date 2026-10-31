#include "libraw/libraw.h"
#include <cstdint>
#include <cstring>
#include <exception>
#include <limits>

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
};

static int fail(char *error, size_t capacity, const char *message) {
    if (capacity) { std::strncpy(error, message, capacity - 1); error[capacity - 1] = 0; }
    return -1;
}

extern "C" int diff_raw_open(const void *bytes, size_t length, int thumbnail,
                             RawView *out, char *error, size_t capacity) {
    auto *result = new RawResult();
    try {
        int status = result->decoder.open_buffer(const_cast<void *>(bytes), length);
        if (status) { delete result; return fail(error, capacity, libraw_strerror(status)); }
        int memory_error = 0;
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
            params.user_qual = 0; // Fast bilinear development; no denoise or sharpening.
            params.med_passes = 0;
            status = result->decoder.unpack();
            if (!status) status = result->decoder.dcraw_process();
            if (!status) result->image = result->decoder.dcraw_make_mem_image(&memory_error);
        }
        if (status || !result->image || memory_error) {
            delete result;
            return fail(error, capacity, libraw_strerror(status ? status : memory_error));
        }
        const auto *image = result->image;
        *out = {result, image->data, image->width, image->height, image->colors,
                image->bits, image->data_size, static_cast<uint32_t>(image->type),
                thumbnail ? static_cast<uint32_t>(result->decoder.imgdata.sizes.flip) : 0u};
        return 0;
    } catch (const std::exception &e) {
        delete result;
        return fail(error, capacity, e.what());
    } catch (...) {
        delete result;
        return fail(error, capacity, "LibRaw decoder exception");
    }
}
extern "C" void diff_raw_close(void *owner) { delete static_cast<RawResult *>(owner); }
