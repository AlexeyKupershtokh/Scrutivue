/*
 * Parallel Nikon HE orchestration and LibRaw virtual hotspot adapters.
 * Adapted from the exact Redlamp LibRaw fork:
 * f4956c404fba0c6035ce9e06353d4b4ffe068fc7.
 * Nikon HE decoder: Copyright (C) 2026 Dmitri Sotnikov.
 * LibRaw processing: Copyright (C) 2019-2025 LibRaw LLC.
 * This adaptation is offered under the same alternative LGPL-2.1 / CDDL-1.0
 * terms; see vendor/libraw/LICENSE.LGPL and vendor/libraw/LICENSE.CDDL.
 * No vendor source, decoder pin, transfer curve or development policy is changed.
 */
#include "parallel_raw.h"
#include "src/decoders/nikon_he/nikon_he_decode.h"
#include "src/decoders/nikon_he/nikon_he_gtli_table.h"
#include "src/decoders/nikon_he/nikon_he_iqx_iqp_lut_data.h"
#include "src/decoders/nikon_he/nikon_he_picture_header.h"
#include "src/decoders/nikon_he/nikon_he_predict_lut.h"
#include <algorithm>
#include <array>
#include <atomic>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <limits>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <type_traits>
#include <vector>

extern "C" int diff_raw_parallel_for(void *, void (*)(void *, uint32_t), uint32_t);

namespace {
template<class Function> void parallel_for(uint32_t count, Function &&function) {
    struct Work {
        Function &function;
        std::atomic<bool> failed{false};
        std::mutex mutex;
        std::exception_ptr error;
    } work{function};
    const auto callback = [](void *opaque, uint32_t index) {
        auto &job = *static_cast<Work *>(opaque);
        if (job.failed.load(std::memory_order_acquire)) return;
        try { job.function(index); }
        catch (...) {
            std::lock_guard<std::mutex> lock(job.mutex);
            if (!job.error) job.error = std::current_exception();
            job.failed.store(true, std::memory_order_release);
        }
    };
    const int status = diff_raw_parallel_for(&work, callback, count);
    if (work.error) std::rethrow_exception(work.error);
    if (status) throw std::runtime_error("Parallel RAW worker failed");
}

uint16_t clipped(int value) { return static_cast<uint16_t>((std::max)(0, (std::min)(65535, value))); }

struct PictureScope {
    explicit PictureScope(const nikon_he::PictureHeader &header) { nikon_he::set_active_picture_header(&header); }
    ~PictureScope() { nikon_he::set_active_picture_header(nullptr); }
};
struct Predecessor {
    nikon_he::PrecinctPredecessorState state;
    ~Predecessor() { state.destroy(); }
};
struct Precinct { const uint8_t *data; size_t size; };

unsigned be16(const uint8_t *p) { return (unsigned(p[0]) << 8) | p[1]; }
size_t be24(const uint8_t *p) { return (size_t(p[0]) << 16) | (size_t(p[1]) << 8) | p[2]; }

std::vector<Precinct> index_precincts(const uint8_t *strip, size_t length, size_t offset, int tiles) {
    if (offset < 6 || offset >= length || be16(strip + offset - 6) != 0xff20 ||
        be16(strip + offset - 4) != 4 || be16(strip + offset - 2) != 0)
        throw std::runtime_error("Invalid Nikon HE initial slice header");
    std::vector<Precinct> result;
    const size_t maximum = size_t(tiles) * 16 + 2;
    result.reserve(maximum);
    size_t cursor = offset;
    while (result.size() < maximum) {
        const size_t remaining = length - cursor;
        if (remaining >= 2 && be16(strip + cursor) == 0xff11) break;
        if (remaining < 3) throw std::runtime_error("Truncated Nikon HE precinct header");
        const size_t size = be24(strip + cursor);
        if (!size) break;
        if (size < 68 || size + 12 > remaining)
            throw std::runtime_error("Invalid Nikon HE precinct span");
        result.push_back({strip + cursor, size});
        cursor += size + 12;
        if ((result.size() & 15u) == 0) {
            const size_t after = length - cursor;
            if ((after >= 2 && be16(strip + cursor) == 0xff11) ||
                (after >= 3 && be24(strip + cursor) == 0)) break;
            if (after < 6 || be16(strip + cursor) != 0xff20 || be16(strip + cursor + 2) != 4 ||
                be16(strip + cursor + 4) != result.size() / 16)
                throw std::runtime_error("Invalid Nikon HE slice boundary");
            cursor += 6;
        }
    }
    const size_t remaining = length - cursor;
    if (!((remaining >= 2 && be16(strip + cursor) == 0xff11) ||
          (remaining >= 3 && be24(strip + cursor) == 0)))
        throw std::runtime_error("Missing Nikon HE end marker");
    return result;
}

std::vector<int32_t> transfer_curve(const nikon_he::PictureHeader &header) {
    const uint8_t *t = header.vendor_tail;
    const int64_t B = (int64_t(t[3] & 15) << 8) | t[4];
    const int64_t A = (int64_t(t[7] & 3) << 16) | (int64_t(t[8]) << 8) | t[9];
    const int64_t C = (int64_t(t[10]) << 8) | t[11];
    std::vector<int32_t> result(nikon_he::kIqxIqpLutSize);
    for (int i = 0; i < nikon_he::kIqxIqpLutSize; ++i) {
        const int64_t d = (i < 65535 ? i : 65535) - C;
        const int64_t g = (d * A + 32768) >> 16;
        const int64_t q = d < 0 ? -d * d : g * g;
        const int64_t raw14 = B + ((q + 163840) >> 18);
        result[i] = int32_t(4 * (std::max)(int64_t(0), (std::min)(int64_t(16383), raw14)));
    }
    return result;
}
} // namespace

bool ParallelRaw::enable_parallel(const void *bytes, size_t length, Cancel cancel, void *context) {
    const auto &d = imgdata;
    const auto &s = d.sizes;
    const auto &u = libraw_internal_data.unpacker_data;
    const auto &io = libraw_internal_data.internal_output_params;
    const char *name = unpack_function_name();
    // Deliberately limited to the tested Nikon HE Bayer path. HE*, other RAWs,
    // crops, half-size development and unknown geometries retain the pinned path.
    if (!bytes || !name || std::strcmp(name, "nikon_he_load_raw()") || d.idata.dng_version ||
        d.makernotes.nikon.NEFCompression != 13 || d.idata.filters <= 1000 || d.idata.colors != 3 ||
        d.idata.is_foveon || io.fuji_width || io.zero_is_bad ||
        s.raw_width < 64 || s.raw_height < 64 || s.raw_width % 8 || s.raw_height % 2 ||
        s.width != s.raw_width || s.height != s.raw_height || s.left_margin || s.top_margin ||
        s.pixel_aspect != 1.0 || uint64_t(s.raw_width) * s.raw_height > 100000000 ||
        (s.raw_pitch && s.raw_pitch != s.raw_width * 2) ||
        d.params.cropbox[2] != (std::numeric_limits<unsigned>::max)() ||
        d.params.cropbox[3] != (std::numeric_limits<unsigned>::max)() ||
        d.params.half_size || d.params.user_qual != 0 || d.params.output_bps != 16 ||
        d.params.output_color != 1 || !d.params.no_auto_bright ||
        u.data_offset < 0 || u.data_size < 64 || uint64_t(u.data_offset) > length ||
        uint64_t(u.data_size) > length - uint64_t(u.data_offset) ||
        uint64_t(u.data_size) > uint64_t((std::numeric_limits<int32_t>::max)())) return false;
    const auto *strip = static_cast<const uint8_t *>(bytes) + size_t(u.data_offset);
    nikon_he::PictureHeader header;
    if (!nikon_he::parse_picture_header(strip, size_t(u.data_size), header) ||
        !nikon_he::is_supported_picture_header(header, size_t(u.data_size)) || !header.has_vendor_tail ||
        header.hdr_width != s.raw_width || header.hdr_height != s.raw_height ||
        header.precinct_offset >= size_t(u.data_size)) return false;
    original_loader_ = load_raw;
    strip_ = strip;
    strip_size_ = size_t(u.data_size);
    cancel_ = cancel;
    cancel_context_ = context;
    enabled_ = true;
    // C++ explicitly permits a derived member pointer's conversion to its
    // non-virtual base. unpack invokes it only on this actual ParallelRaw object.
    // Keep LibRaw's allocation, black/mask handling and retained-raw bookkeeping.
    load_raw = static_cast<void (LibRaw::*)()>(&ParallelRaw::load_nef_parallel);
    return true;
}

int ParallelRaw::get_decoder_info(libraw_decoder_info_t *info) {
    if (!enabled_) return LibRaw::get_decoder_info(info);
    const auto parallel = load_raw;
    load_raw = original_loader_;
    const int status = LibRaw::get_decoder_info(info);
    load_raw = parallel;
    return status;
}

void ParallelRaw::check_cancelled() const {
    if (cancel_ && cancel_(cancel_context_)) throw LIBRAW_EXCEPTION_CANCELLED_BY_CALLBACK;
}

void ParallelRaw::load_nef_parallel() {
    check_cancelled();
    const int width = imgdata.sizes.raw_width, height = imgdata.sizes.raw_height;
    if (!imgdata.rawdata.raw_image) throw LIBRAW_EXCEPTION_ALLOC;
    if (strip_size_ > uint64_t(imgdata.rawparams.max_raw_memory_mb) * 1024 * 1024)
        throw LIBRAW_EXCEPTION_ALLOC;
    nikon_he::PictureHeader header;
    if (!nikon_he::parse_picture_header(strip_, strip_size_, header)) throw LIBRAW_EXCEPTION_DECODE_RAW;
    const int tiles = (height + 63) / 64;
    const auto precincts = index_precincts(strip_, strip_size_, header.precinct_offset, tiles);
    const auto transfer = transfer_curve(header);
    nikon_he::SubbandConfig config[26];
    nikon_he::LayoutInfo layout;
    nikon_he::compute_subband_layout(width / 2, config, layout);
    const int stride = nikon_he::compute_buf_stripe_ints(&layout);
    const int band = nikon_he::compute_kband(&layout);
    if (stride <= 0 || band < width / 2 || size_t(3) * band + width / 2 > size_t(stride))
        throw LIBRAW_EXCEPTION_DECODE_RAW;
    const size_t tile_size = size_t(nikon_he::kStripesPerTile) * stride;
    const uint64_t workspace = uint64_t(tiles) * (2 * tile_size + 2 * size_t(stride)) * sizeof(int32_t);
    if (workspace + uint64_t(width) * height * 2 >
        uint64_t(imgdata.rawparams.max_raw_memory_mb) * 1024 * 1024) throw LIBRAW_EXCEPTION_ALLOC;
    std::vector<int32_t> coefficients(size_t(tiles) * tile_size, 0);
    std::vector<int32_t> scratch(size_t(tiles) * tile_size, 0);
    std::vector<int32_t> overflows(size_t(tiles) * 2 * stride, 0);
    const uint8_t *prediction = nikon_he::build_prediction_lookup_table();

    // Each tile's lifting arithmetic does not read incoming overflow: it only
    // copies it to the first two coefficient rows. Decode independently, retain
    // each outgoing carry, then repair those rows after the synchronous join.
    parallel_for(tiles, [&](uint32_t tile) {
        check_cancelled();
        PictureScope active(header); // GTLI header state is thread-local.
        const size_t first = size_t(tile) * 16;
        const int count = first < precincts.size() ? int((std::min)(size_t(nikon_he::kPrecinctsPerTile), precincts.size() - first)) : 0;
        std::array<const uint8_t *, nikon_he::kPrecinctsPerTile> pointers{};
        std::array<size_t, nikon_he::kPrecinctsPerTile> sizes{};
        for (int p = 0; p < count; ++p) { pointers[p] = precincts[first + p].data; sizes[p] = precincts[first + p].size; }
        Predecessor predecessor;
        predecessor.state.init(config);
        auto decoded = nikon_he::decode_tile(pointers.data(), sizes.data(), width, config,
            predecessor.state, prediction, coefficients.data(), tile,
            overflows.data() + size_t(tile) * 2 * stride, tile == 0, tile == tiles - 1, count);
        if (!decoded.success || decoded.precincts_decoded != count) throw LIBRAW_EXCEPTION_DECODE_RAW;
    });
    for (int tile = 1; tile < tiles; ++tile)
        std::memcpy(coefficients.data() + size_t(tile) * tile_size,
            overflows.data() + size_t(tile - 1) * 2 * stride, size_t(2) * stride * sizeof(int32_t));
    parallel_for(tiles, [&](uint32_t tile) {
        check_cancelled();
        const bool first = tile == 0, last = tile == tiles - 1;
        const int rows = last && height % 64 ? height % 64 / 2 : nikon_he::kStripesPerTile;
        const auto *from = coefficients.data() + size_t(tile) * tile_size;
        auto *to = scratch.data() + size_t(tile) * tile_size;
        nikon_he::step1_merge_4_to_2(from + band, from, from + 3 * band, from + 2 * band,
            rows, width / 2, stride, stride, to, to + band, stride, first, last);
    });
    parallel_for(tiles, [&](uint32_t tile) {
        check_cancelled();
        const bool first = tile == 0, last = tile == tiles - 1;
        const int rows = last && height % 64 ? height % 64 / 2 : nikon_he::kStripesPerTile;
        const auto *coeff = coefficients.data() + size_t(tile) * tile_size;
        const auto *step1 = scratch.data() + size_t(tile) * tile_size;
        nikon_he::step2_bayer_rows(coeff + band, step1, step1 + band, coeff + 2 * band,
            rows, width / 2, stride, stride, transfer.data(), imgdata.rawdata.raw_image,
            width, tile * 64, first, last);
    });
    check_cancelled();
    imgdata.color.maximum = 16383;
}

void ParallelRaw::copy_bayer(unsigned short black[4], unsigned short *maximum) {
    if (!enabled_ || libraw_internal_data.internal_output_params.shrink) return LibRaw::copy_bayer(black, maximum);
    const auto &s = imgdata.sizes;
    const int rows = (std::max)(0, (std::min)(int(s.height), int(s.raw_height) - int(s.top_margin)));
    std::vector<uint16_t> maxima(rows, 0);
    parallel_for(rows, [&](uint32_t row) {
        check_cancelled();
        uint16_t local = 0;
        for (int col = 0; col < s.width && col + s.left_margin < s.raw_width; ++col) {
            uint16_t value = imgdata.rawdata.raw_image[(row + s.top_margin) * s.raw_pitch / 2 + col + s.left_margin];
            const int color = fcol(row, col);
            if (value > black[color]) { value -= black[color]; local = (std::max)(local, value); }
            else value = 0;
            imgdata.image[size_t(row) * s.iwidth + col][color] = value;
        }
        maxima[row] = local;
    });
    for (auto value : maxima) *maximum = (std::max)(*maximum, value);
}

void ParallelRaw::scale_colors_loop(float multipliers[4]) {
    if (!enabled_) return LibRaw::scale_colors_loop(multipliers);
    const auto &s = imgdata.sizes;
    const auto &black = imgdata.color.cblack;
    parallel_for(s.iheight, [&](uint32_t row) {
        check_cancelled();
        for (unsigned col = 0; col < s.iwidth; ++col) {
            const size_t i = size_t(row) * s.iwidth + col;
            for (unsigned c = 0; c < 4; ++c) {
                int value = imgdata.image[i][c];
                if (black[4] && black[5]) {
                    if (!value) continue;
                    value -= black[6 + row % black[4] * black[5] + col % black[5]];
                    value -= black[c];
                } else if (black[0] || black[1] || black[2] || black[3]) {
                    if (!value) continue;
                    value -= black[c];
                }
                value = int(value * multipliers[c]);
                imgdata.image[i][c] = clipped(value);
            }
        }
    });
}

void ParallelRaw::lin_interpolate_loop(int *code, int size) {
    if (!enabled_) return LibRaw::lin_interpolate_loop(code, size);
    const auto &s = imgdata.sizes;
    parallel_for(s.height - 2, [&](uint32_t index) {
        check_cancelled();
        const int row = index + 1;
        for (int col = 1; col < s.width - 1; ++col) {
            int sums[4]{};
            auto *pixel = imgdata.image[size_t(row) * s.width + col];
            const int *instruction = code + (((row % size) * 16 + col % size) * 32);
            // Only native CFA channels are read; interpolated channels written
            // by another row are never inputs. Borders were completed serially.
            for (int i = *instruction++; i--; instruction += 3)
                sums[instruction[2]] += pixel[instruction[0]] << instruction[1];
            for (int i = imgdata.idata.colors; --i; instruction += 2)
                pixel[instruction[0]] = sums[instruction[0]] * instruction[1] >> 8;
        }
    });
}

void ParallelRaw::convert_to_rgb_loop(float matrix[3][4]) {
    if (!enabled_ || imgdata.idata.colors != 3) return LibRaw::convert_to_rgb_loop(matrix);
    constexpr unsigned block_rows = 32;
    constexpr size_t histogram_size = LIBRAW_HISTOGRAM_SIZE * 4;
    const auto &s = imgdata.sizes;
    const unsigned blocks = (s.height + block_rows - 1) / block_rows;
    std::vector<int> histograms(size_t(blocks) * histogram_size, 0);
    const bool raw_color = libraw_internal_data.internal_output_params.raw_color != 0;
    parallel_for(blocks, [&](uint32_t block) {
        check_cancelled();
        int *histogram = histograms.data() + size_t(block) * histogram_size;
        const unsigned end = (std::min)(unsigned(s.height), (block + 1) * block_rows);
        for (unsigned row = block * block_rows; row < end; ++row) {
            for (unsigned col = 0; col < s.width; ++col) {
                auto *pixel = imgdata.image[size_t(row) * s.width + col];
                if (!raw_color) {
                    float out[3];
                    for (int c = 0; c < 3; ++c)
                        out[c] = matrix[c][0] * pixel[0] + matrix[c][1] * pixel[1] + matrix[c][2] * pixel[2];
                    for (int c = 0; c < 3; ++c) pixel[c] = clipped(int(out[c]));
                }
                for (unsigned c = 0; c < 3; ++c) ++histogram[c * LIBRAW_HISTOGRAM_SIZE + (pixel[c] >> 3)];
            }
        }
    });
    auto *histogram = &libraw_internal_data.output_data.histogram[0][0];
    std::fill_n(histogram, histogram_size, 0);
    for (unsigned block = 0; block < blocks; ++block)
        for (size_t i = 0; i < histogram_size; ++i) histogram[i] += histograms[size_t(block) * histogram_size + i];
}

libraw_processed_image_t *ParallelRaw::dcraw_make_mem_image(int *error) {
    if (!enabled_ || imgdata.idata.colors != 3 || imgdata.params.output_bps != 16 || !imgdata.params.no_auto_bright)
        return LibRaw::dcraw_make_mem_image(error);
    if ((imgdata.progress_flags & LIBRAW_PROGRESS_THUMB_MASK) < LIBRAW_PROGRESS_PRE_INTERPOLATE) {
        if (error) *error = LIBRAW_OUT_OF_ORDER_CALL;
        return nullptr;
    }
    int width, height, colors, bits;
    get_mem_image_format(&width, &height, &colors, &bits);
    const size_t bytes = size_t(width) * height * colors * 2;
    auto *image = static_cast<libraw_processed_image_t *>(::malloc(sizeof(libraw_processed_image_t) + bytes));
    if (!image) { if (error) *error = ENOMEM; return nullptr; }
    std::unique_ptr<libraw_processed_image_t, void(*)(void *)> owner(image, ::free);
    std::memset(image, 0, sizeof(*image));
    image->type = LIBRAW_IMAGE_BITMAP;
    image->width = width; image->height = height; image->colors = colors; image->bits = bits; image->data_size = bytes;
    if (libraw_internal_data.output_data.histogram)
        gamma_curve(imgdata.params.gamm[0], imgdata.params.gamm[1], 2, int(65536 / imgdata.params.bright));
    auto *output = reinterpret_cast<uint16_t *>(image->data);
    const auto &s = imgdata.sizes;
    constexpr unsigned tile_size = 32;
    parallel_for((height + tile_size - 1) / tile_size, [&](uint32_t block) {
        check_cancelled();
        const unsigned end_row = (std::min)(unsigned(height), (block + 1) * tile_size);
        for (unsigned left = 0; left < unsigned(width); left += tile_size) {
            const unsigned right = (std::min)(unsigned(width), left + tile_size);
            for (unsigned row = block * tile_size; row < end_row; ++row) {
                for (unsigned col = left; col < right; ++col) {
                    unsigned y = row, x = col;
                    if (s.flip & 4) std::swap(y, x);
                    if (s.flip & 2) y = s.height - 1 - y;
                    if (s.flip & 1) x = s.width - 1 - x;
                    const auto *source = imgdata.image[size_t(y) * s.width + x];
                    auto *to = output + (size_t(row) * width + col) * 3;
                    for (unsigned c = 0; c < 3; ++c) to[c] = imgdata.color.curve[source[c]];
                }
            }
        }
    });
    check_cancelled();
    if (error) *error = 0;
    return owner.release();
}

// Small deterministic native/reference checks, called only by Rust tests. The
// base implementations remain independent oracles; no real image is required.
void ParallelRaw::test_kernels() {
    struct Serial : LibRaw {
        using LibRaw::copy_bayer;
        using LibRaw::scale_colors_loop;
        using LibRaw::lin_interpolate;
        using LibRaw::convert_to_rgb_loop;
        using LibRaw::calloc;
    };
    auto require = [](bool condition, const char *message) {
        if (!condition) throw std::runtime_error(message);
    };
    for (auto size : {std::array<unsigned,2>{1,1}, {2,3}, {5,3}, {32,33}, {65,49}}) {
        auto serial = std::make_unique<Serial>();
        auto parallel = std::make_unique<ParallelRaw>();
        const unsigned width = size[0], height = size[1];
        const size_t pixels = size_t(width) * height;
        for (LibRaw *decoder : {static_cast<LibRaw *>(serial.get()), static_cast<LibRaw *>(parallel.get())}) {
            auto &d = decoder->imgdata;
            d.sizes.width = d.sizes.iwidth = d.sizes.raw_width = width;
            d.sizes.height = d.sizes.iheight = d.sizes.raw_height = height;
            d.sizes.raw_pitch = width * 2;
            d.sizes.flip = 0;
            d.sizes.pixel_aspect = 1;
            d.idata.colors = 3;
            d.idata.filters = 0x94949494u;
            d.params.output_bps = 16;
            d.params.no_auto_bright = 1;
            d.progress_flags = LIBRAW_PROGRESS_CONVERT_RGB | LIBRAW_PROGRESS_PRE_INTERPOLATE;
        }
        serial->imgdata.image = static_cast<unsigned short (*)[4]>(serial->calloc(pixels, 8));
        parallel->imgdata.image = static_cast<unsigned short (*)[4]>(parallel->calloc(pixels, 8));
        parallel->enabled_ = true;
        std::vector<uint16_t> raw(pixels);
        for (size_t i = 0; i < pixels; ++i) raw[i] = i % 7 ? uint16_t(i * 17431 + 50123) : 0;
        serial->imgdata.rawdata.raw_image = parallel->imgdata.rawdata.raw_image = raw.data();
        unsigned short black[4]{0,13,500,65535}, amax = 23, bmax = 23;
        serial->copy_bayer(black, &amax);
        parallel->copy_bayer(black, &bmax);
        require(amax == bmax && !std::memcmp(serial->imgdata.image, parallel->imgdata.image, pixels * 8), "Parallel Bayer copy mismatch");
        if (width >= 3 && height >= 3) {
            serial->lin_interpolate();
            parallel->lin_interpolate();
            require(!std::memcmp(serial->imgdata.image, parallel->imgdata.image, pixels * 8), "Parallel bilinear interpolation mismatch");
        }
        auto fill = [&] {
            for (size_t i = 0; i < pixels; ++i)
                for (unsigned c = 0; c < 4; ++c)
                    serial->imgdata.image[i][c] = parallel->imgdata.image[i][c] = (i + c) % 7 ? uint16_t(i * 17431 + c * 923 + 50123) : 0;
        };
        for (unsigned mode = 0; mode < 3; ++mode) {
            fill();
            for (LibRaw *decoder : {static_cast<LibRaw *>(serial.get()), static_cast<LibRaw *>(parallel.get())}) {
                std::fill_n(decoder->imgdata.color.cblack, LIBRAW_CBLACK_SIZE, 0);
                if (mode) for (unsigned c = 0; c < 4; ++c) decoder->imgdata.color.cblack[c] = c * 123;
                if (mode == 2) {
                    decoder->imgdata.color.cblack[4] = 2;
                    decoder->imgdata.color.cblack[5] = 3;
                    for (unsigned i = 0; i < 6; ++i) decoder->imgdata.color.cblack[6 + i] = i * 17;
                }
            }
            float multipliers[4]{0.8f,1.17f,2.25f,0.6f};
            serial->scale_colors_loop(multipliers);
            parallel->scale_colors_loop(multipliers);
            require(!std::memcmp(serial->imgdata.image, parallel->imgdata.image, pixels * 8), "Parallel scale/black correction mismatch");
        }
        auto *serial_internal = serial->get_internal_data_pointer();
        auto *parallel_internal = parallel->get_internal_data_pointer();
        serial_internal->output_data.histogram = static_cast<int (*)[LIBRAW_HISTOGRAM_SIZE]>(serial->calloc(4 * LIBRAW_HISTOGRAM_SIZE, sizeof(int)));
        parallel_internal->output_data.histogram = static_cast<int (*)[LIBRAW_HISTOGRAM_SIZE]>(parallel->calloc(4 * LIBRAW_HISTOGRAM_SIZE, sizeof(int)));
        for (bool raw_color : {false,true}) {
            fill();
            serial_internal->internal_output_params.raw_color = parallel_internal->internal_output_params.raw_color = raw_color;
            float matrix[3][4]{{1.2f,-0.2f,0.13f,0},{-0.1f,0.91f,0.08f,0},{0.03f,0.11f,0.92f,0}};
            serial->convert_to_rgb_loop(matrix);
            parallel->convert_to_rgb_loop(matrix);
            require(!std::memcmp(serial->imgdata.image, parallel->imgdata.image, pixels * 8), "Parallel RGB conversion mismatch");
            require(!std::memcmp(serial_internal->output_data.histogram, parallel_internal->output_data.histogram, 4 * LIBRAW_HISTOGRAM_SIZE * sizeof(int)), "Parallel histogram mismatch");
        }
        for (int flip = 0; flip < 8; ++flip) {
            for (float bright : {0.7f,1.f}) {
                serial->imgdata.sizes.flip = parallel->imgdata.sizes.flip = flip;
                serial->imgdata.params.bright = parallel->imgdata.params.bright = bright;
                int aerr = 0, berr = 0;
                auto a = serial->dcraw_make_mem_image(&aerr);
                auto b = parallel->dcraw_make_mem_image(&berr);
                const bool equal = a && b && !aerr && !berr && a->width == b->width && a->height == b->height &&
                    a->data_size == b->data_size && !std::memcmp(a->data, b->data, a->data_size);
                LibRaw::dcraw_clear_mem(a); LibRaw::dcraw_clear_mem(b);
                require(equal, "Parallel gamma/orientation mismatch");
            }
        }
        serial->imgdata.rawdata.raw_image = parallel->imgdata.rawdata.raw_image = nullptr;
    }
    std::vector<uint8_t> stream{0xff,0x20,0,4,0,0};
    for (unsigned p = 0; p < 18; ++p) {
        const size_t at = stream.size();
        stream.resize(at + 80, 0);
        stream[at + 2] = 68;
        if (p == 15) stream.insert(stream.end(), {0xff,0x20,0,4,0,1});
    }
    stream.insert(stream.end(), {0xff,0x11});
    require(index_precincts(stream.data(),stream.size(),6,1).size() == 18, "Nikon HE precinct indexing mismatch");
    for (size_t bad : {size_t(0),size_t(8),size_t(6 + 16 * 80),stream.size() - 1}) {
        auto damaged = stream;
        damaged[bad] ^= 0x40;
        bool rejected = false;
        try { index_precincts(damaged.data(),damaged.size(),6,1); } catch (const std::exception &) { rejected = true; }
        require(rejected, "Damaged Nikon HE index was accepted");
    }
}

extern "C" int diff_raw_test_parallel_kernels(char *error, size_t capacity) {
    try { ParallelRaw::test_kernels(); return 0; }
    catch (const std::exception &failure) {
        if (capacity) { std::strncpy(error,failure.what(),capacity - 1); error[capacity - 1] = 0; }
    }
    catch (...) { if (capacity) { std::strncpy(error,"Native RAW kernel test failed",capacity - 1); error[capacity - 1] = 0; } }
    return -1;
}
