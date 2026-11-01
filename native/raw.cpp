#include "libraw/libraw.h"
#include "parallel_raw.h"
#include <cstdint>
#include <cstring>
#include <exception>
#include <limits>
#include <chrono>
#include <cmath>
#include <memory>

// Own the LibRaw allocation until Rust finishes reading it. Each job has its own instance.
struct RawResult {
    ParallelRaw decoder;
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

static void configure_development(LibRaw &decoder, bool region) {
    auto &params = decoder.imgdata.params;
    params.output_bps = 16;
    params.output_color = 1; // sRGB, consistent across all RAWs in the comparison.
    params.use_camera_wb = 1;
    params.no_auto_bright = 1;
    // A tile must keep the metadata white level, not normalize to a local peak.
    if (region) params.adjust_maximum_thr = 0.f;
    params.user_qual = 0;
    params.med_passes = 0;
}

// Header-only, fail-closed proof for the deliberately narrow pointwise RGB path.
// The caller changes only the original JXL compression tag for header reading;
// neither input is unpacked here. The pinned LibRaw implementation is unchanged.
extern "C" unsigned diff_raw_matching_tile_policy(const void *original, size_t original_length,
                                                  const void *region, size_t region_length,
                                                  unsigned width, unsigned height, unsigned orientation) {
    try {
        auto full_owner = std::make_unique<LibRaw>();
        auto tile_owner = std::make_unique<LibRaw>();
        auto &full = *full_owner;
        auto &tile = *tile_owner;
        const int full_status = full.open_buffer(const_cast<void *>(original), original_length);
        const int tile_status = tile.open_buffer(const_cast<void *>(region), region_length);
        const bool trace = std::getenv("SCRUTIVUE_TILE_POLICY_TRACE") != nullptr;
        if (full_status || tile_status) {
            if (trace) std::fprintf(stderr, "tile policy header status: full=%d region=%d\n", full_status, tile_status);
            return 0;
        }
        configure_development(full, false);
        configure_development(tile, true);
        auto pointwise = [](LibRaw &decoder) {
            const auto &data = decoder.imgdata;
            const auto &color = data.color;
            const auto &sizes = data.sizes;
            const auto &params = data.params;
            const auto &internal = decoder.get_internal_data_pointer()->internal_output_params;
            const auto &unpacker = decoder.get_internal_data_pointer()->unpacker_data;
            if (!data.idata.dng_version || data.idata.filters || data.idata.colors != 3 ||
                data.idata.is_foveon || internal.zero_is_bad || internal.mix_green ||
                internal.fuji_width || internal.shrink || sizes.pixel_aspect != 1.0 ||
                sizes.left_margin || sizes.top_margin || sizes.width != sizes.raw_width ||
                sizes.height != sizes.raw_height || sizes.width < 32 || sizes.height < 32 ||
                color.black || color.as_shot_wb_applied ||
                !color.maximum || color.maximum > 65535 || unpacker.tiff_compress != 1 ||
                unpacker.tiff_bps != 16 || unpacker.tiff_samples != 3 ||
                params.use_auto_wb || params.threshold || params.green_matching ||
                params.highlight || params.exp_correc || params.camera_profile ||
                params.aber[0] != 1.0 || params.aber[2] != 1.0) return false;
            for (unsigned c = 0; c < LIBRAW_CBLACK_SIZE; ++c)
                // The last slot records the DNG BlackLevel value count, not
                // another black correction (identify.cpp, pinned LibRaw).
                if (c == LIBRAW_CBLACK_SIZE - 1) { if (color.cblack[c] > 3) return false; }
                else if (c == 4 || c == 5) { if (color.cblack[c] > 1) return false; }
                else if (color.cblack[c]) return false;
            for (unsigned c = 0; c < 3; ++c)
                if (!std::isfinite(color.cam_mul[c]) || color.cam_mul[c] <= 0.00001f) return false;
            for (float value : color.pre_mul) if (!std::isfinite(value)) return false;
            for (const auto &row : color.rgb_cam)
                for (float value : row) if (!std::isfinite(value)) return false;
            for (unsigned c = 0; c < 65536; ++c)
                if (color.curve[c] != c) return false;
            return true;
        };
        if (trace) {
            auto report = [](const char *name, LibRaw &decoder) {
                const auto &d = decoder.imgdata;
                const auto *i = decoder.get_internal_data_pointer();
                std::fprintf(stderr, "tile policy %s: foveon=%u dng=%u colors=%u filters=%u raw=%ux%u active=%ux%u margins=%u,%u aspect=%g flip=%d max=%u black=%u wb=%g,%g,%g internal=%u,%u,%u,%u,%u tiff=%u,%u,%u\n",
                    name, unsigned(d.idata.is_foveon), d.idata.dng_version, d.idata.colors, d.idata.filters,
                    d.sizes.raw_width, d.sizes.raw_height, d.sizes.width, d.sizes.height,
                    d.sizes.left_margin, d.sizes.top_margin, d.sizes.pixel_aspect, d.sizes.flip,
                    d.color.maximum, d.color.black, d.color.cam_mul[0], d.color.cam_mul[1], d.color.cam_mul[2],
                    i->internal_output_params.raw_color, i->internal_output_params.mix_green,
                    i->internal_output_params.zero_is_bad, i->internal_output_params.shrink, i->internal_output_params.fuji_width,
                    i->unpacker_data.tiff_compress, i->unpacker_data.tiff_bps, i->unpacker_data.tiff_samples);
                for (unsigned c = 0; c < LIBRAW_CBLACK_SIZE; ++c)
                    if (d.color.cblack[c]) std::fprintf(stderr, "tile policy %s cblack[%u]=%u\n", name, c, d.color.cblack[c]);
            };
            report("full", full); report("region", tile);
            std::fprintf(stderr, "tile policy checks: full=%d region=%d wb=%d premul=%d matrix=%d white=%d gamma=%d\n",
                pointwise(full), pointwise(tile), std::memcmp(full.imgdata.color.cam_mul, tile.imgdata.color.cam_mul, sizeof full.imgdata.color.cam_mul),
                std::memcmp(full.imgdata.color.pre_mul, tile.imgdata.color.pre_mul, sizeof full.imgdata.color.pre_mul),
                std::memcmp(full.imgdata.color.rgb_cam, tile.imgdata.color.rgb_cam, sizeof full.imgdata.color.rgb_cam),
                std::memcmp(full.imgdata.color.white, tile.imgdata.color.white, sizeof full.imgdata.color.white),
                std::memcmp(full.imgdata.params.gamm, tile.imgdata.params.gamm, sizeof full.imgdata.params.gamm));
        }
        if (!pointwise(full) || !pointwise(tile) || full.imgdata.sizes.width != width ||
            full.imgdata.sizes.height != height) return 0;
        static const int tiff_flip[] = {0, 0, 1, 3, 2, 4, 6, 7, 5};
        if (orientation < 1 || orientation > 8 || full.imgdata.sizes.flip != tiff_flip[orientation] ||
            tile.imgdata.sizes.flip != 0) return 0;
        const auto &a = full.imgdata.color;
        const auto &b = tile.imgdata.color;
        if (a.maximum != b.maximum ||
            full.get_internal_data_pointer()->internal_output_params.raw_color !=
                tile.get_internal_data_pointer()->internal_output_params.raw_color ||
            std::memcmp(a.cam_mul, b.cam_mul, sizeof a.cam_mul) ||
            std::memcmp(a.pre_mul, b.pre_mul, sizeof a.pre_mul) ||
            std::memcmp(a.rgb_cam, b.rgb_cam, sizeof a.rgb_cam) ||
            std::memcmp(a.white, b.white, sizeof a.white) ||
            std::memcmp(full.imgdata.params.gamm, tile.imgdata.params.gamm, sizeof full.imgdata.params.gamm)) return 0;
        return a.maximum;
    } catch (...) { return 0; }
}

static int raw_open_cancellable(const void *bytes, size_t length, int thumbnail, bool region, bool parallel,
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
            configure_development(result->decoder, region);
            if (parallel) result->decoder.enable_parallel(bytes, length, callback, context);
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
    return raw_open_cancellable(bytes, length, thumbnail, false, true, out, error, capacity, callback, context);
}
extern "C" int diff_raw_open_region_cancellable(const void *bytes, size_t length,
                             RawView *out, char *error, size_t capacity,
                             CancelFn callback, void *context) {
    return raw_open_cancellable(bytes, length, 0, true, false, out, error, capacity, callback, context);
}
extern "C" int diff_raw_open_legacy_cancellable(const void *bytes, size_t length, int thumbnail,
                             RawView *out, char *error, size_t capacity,
                             CancelFn callback, void *context) {
    return raw_open_cancellable(bytes, length, thumbnail, false, false, out, error, capacity, callback, context);
}
extern "C" int diff_raw_open(const void *bytes, size_t length, int thumbnail,
                             RawView *out, char *error, size_t capacity) {
    return diff_raw_open_cancellable(bytes, length, thumbnail, out, error, capacity, nullptr, nullptr);
}
extern "C" void diff_raw_close(void *owner) { delete static_cast<RawResult *>(owner); }
// Read-only diagnostics for independent raw/RGB reference tests.
extern "C" int diff_raw_uses_parallel(void *owner) {
    return owner && static_cast<RawResult *>(owner)->decoder.parallel_enabled();
}
extern "C" const unsigned short *diff_raw_sensor_samples(void *owner, size_t *count) {
    if (!owner || !count) return nullptr;
    const auto &raw = static_cast<RawResult *>(owner)->decoder.imgdata.rawdata;
    if (!raw.raw_image || raw.sizes.raw_pitch != raw.sizes.raw_width * 2) return nullptr;
    *count = size_t(raw.sizes.raw_width) * raw.sizes.raw_height;
    return raw.raw_image;
}
