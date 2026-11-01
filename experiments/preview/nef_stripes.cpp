/*
 * Experimental partial Nikon HE / HE* strip orchestration.
 * Adapted from nikon_he_decode.cpp and nikon_he_decoder.cpp in the pinned
 * Redlamp LibRaw fork, commit f4956c404fba0c6035ce9e06353d4b4ffe068fc7.
 * Original decoder and transfer-curve logic:
 * Copyright (C) 2026 Dmitri Sotnikov.
 *
 * LibRaw is free software; you can redistribute it and/or modify it under
 * either GNU LESSER GENERAL PUBLIC LICENSE version 2.1 or COMMON DEVELOPMENT
 * AND DISTRIBUTION LICENSE (CDDL) Version 1.0, as you choose. See
 * vendor/libraw/LICENSE.LGPL and vendor/libraw/LICENSE.CDDL.
 * This adaptation is offered under those same alternative terms.
 */

#include "nef_stripes.h"

#include "../../vendor/libraw/src/decoders/nikon_he/nikon_he_decode.h"
#include "../../vendor/libraw/src/decoders/nikon_he/nikon_he_gtli_table.h"
#include "../../vendor/libraw/src/decoders/nikon_he/nikon_he_iqx_iqp_lut_data.h"
#include "../../vendor/libraw/src/decoders/nikon_he/nikon_he_picture_header.h"
#include "../../vendor/libraw/src/decoders/nikon_he/nikon_he_predict_lut.h"

#include <algorithm>
#include <chrono>
#include <cstring>
#include <limits>
#include <new>
#include <stdexcept>

namespace {

using Clock = std::chrono::steady_clock;

double elapsed_ms(Clock::time_point start) {
    return std::chrono::duration<double, std::milli>(Clock::now() - start).count();
}

[[noreturn]] void fail(const char* message) {
    throw std::runtime_error(message);
}

size_t checked_add(size_t a, size_t b) {
    if (a > (std::numeric_limits<size_t>::max)() - b)
        fail("Nikon HE stripe buffer size overflow.");
    return a + b;
}

size_t checked_mul(size_t a, size_t b) {
    if (b && a > (std::numeric_limits<size_t>::max)() / b)
        fail("Nikon HE stripe buffer size overflow.");
    return a * b;
}

unsigned be16(const uint8_t* p) {
    return (static_cast<unsigned>(p[0]) << 8) | p[1];
}

size_t be24(const uint8_t* p) {
    return (static_cast<size_t>(p[0]) << 16)
         | (static_cast<size_t>(p[1]) << 8) | p[2];
}

class ActivePictureHeaderScope {
public:
    explicit ActivePictureHeaderScope(const nikon_he::PictureHeader& ph) {
        nikon_he::set_active_picture_header(&ph);
    }
    ~ActivePictureHeaderScope() {
        nikon_he::set_active_picture_header(nullptr);
    }
    ActivePictureHeaderScope(const ActivePictureHeaderScope&) = delete;
    ActivePictureHeaderScope& operator=(const ActivePictureHeaderScope&) = delete;
};

// The pinned predecessor type has destroy(), but no destructor. Keep all
// normally initialized per-tile state scoped instead of inheriting its leak.
class PredecessorScope {
public:
    nikon_he::PrecinctPredecessorState state;
    ~PredecessorScope() { state.destroy(); }
    PredecessorScope() = default;
    PredecessorScope(const PredecessorScope&) = delete;
    PredecessorScope& operator=(const PredecessorScope&) = delete;
};

struct PrecinctRef {
    const uint8_t* data;
    // Exactly the sz argument used by the production orchestrator, NOT sz+12.
    size_t payload_size;
};

std::vector<PrecinctRef> index_precincts(
    const uint8_t* strip, size_t length, size_t offset, int total_tiles) {
    // parse_picture_header skips SLH by its declared length; check the actual
    // first slice marker, length and slice number too.
    if (offset < 6 || offset >= length ||
        be16(strip + offset - 6) != 0xff20 ||
        be16(strip + offset - 4) != 4 || be16(strip + offset - 2) != 0)
        fail("Invalid Nikon HE first slice header.");

    const size_t maximum = checked_add(checked_mul(total_tiles, 16), 2);
    std::vector<PrecinctRef> refs;
    refs.reserve(maximum);
    size_t cursor = offset;
    while (refs.size() < maximum) {
        const size_t remaining = length - cursor;
        if (remaining == 2 && be16(strip + cursor) == 0xff11)
            break;
        if (remaining < 3)
            fail("Truncated Nikon HE precinct length header.");
        const size_t sz = be24(strip + cursor);
        if (sz == 0) // The production decoder also recognizes a zero sentinel.
            break;
        if (sz < 68 || checked_add(sz, 12) > remaining)
            fail("Invalid or truncated Nikon HE precinct span.");
        refs.push_back({strip + cursor, sz});
        cursor += sz + 12;

        if ((refs.size() & 15u) == 0) {
            const size_t after = length - cursor;
            if (after == 2 && be16(strip + cursor) == 0xff11)
                break;
            if (after >= 3 && be24(strip + cursor) == 0)
                break;
            if (after < 6 || be16(strip + cursor) != 0xff20 ||
                be16(strip + cursor + 2) != 4 ||
                be16(strip + cursor + 4) != refs.size() / 16)
                fail("Invalid Nikon HE slice marker, length or index.");
            cursor += 6;
        }
    }

    // Do not infer a minimum count from sensor height plus overlap. Production
    // passes each tile's actual available precinct count to decode_tile, whose
    // zero-initialized tile buffer and tail-state flush support shorter tails.
    const size_t remaining = length - cursor;
    if (!((remaining >= 2 && be16(strip + cursor) == 0xff11) ||
          (remaining >= 3 && be24(strip + cursor) == 0)))
        fail("Missing Nikon HE end marker or precinct sentinel.");
    return refs;
}

void make_transfer_lut(const nikon_he::PictureHeader& ph,
                       std::vector<int32_t>& transfer) {
    // Identical integer transfer curve to nikon_he_decoder.cpp:100-127.
    // Do not substitute the historical approximate iqx_iqp_lut() table.
    const uint8_t* t = ph.vendor_tail;
    const int64_t B = (static_cast<int64_t>(t[3] & 15) << 8) | t[4];
    const int64_t A = (static_cast<int64_t>(t[7] & 3) << 16)
                    | (static_cast<int64_t>(t[8]) << 8) | t[9];
    const int64_t C = (static_cast<int64_t>(t[10]) << 8) | t[11];
    transfer.resize(nikon_he::kIqxIqpLutSize);
    for (int i = 0; i < nikon_he::kIqxIqpLutSize; ++i) {
        const int64_t d = (i < 65535 ? i : 65535) - C;
        int64_t q;
        if (d < 0) {
            q = -d * d;
        } else {
            const int64_t g = (d * A + 32768) >> 16;
            q = g * g;
        }
        int64_t raw14 = B + ((q + 163840) >> 18);
        raw14 = raw14 < 0 ? 0 : (raw14 > 16383 ? 16383 : raw14);
        transfer[i] = static_cast<int32_t>(4 * raw14);
    }
}

NefStripeResult decode_impl(const uint8_t* strip, size_t length,
                            int width, int height,
                            const std::array<unsigned, 4>& roi) {
    NefStripeResult result;
    const auto header_start = Clock::now();
    if (!strip || length < 64 ||
        length > static_cast<size_t>((std::numeric_limits<int32_t>::max)()))
        fail("Invalid Nikon HE compressed strip buffer or length.");
    if (width < 64 || height < 64 || width > 65535 || height > 65535 ||
        width % 8 != 0 || (height & 1))
        fail("Unsupported Nikon HE raw dimensions; full sensor dimensions are required.");
    const unsigned sensor_w = static_cast<unsigned>(width);
    const unsigned sensor_h = static_cast<unsigned>(height);
    if (!roi[2] || !roi[3] || roi[0] >= sensor_w || roi[1] >= sensor_h ||
        roi[2] > sensor_w - roi[0] || roi[3] > sensor_h - roi[1])
        fail("Nikon HE raw ROI is empty or outside the sensor dimensions.");

    nikon_he::PictureHeader ph;
    if (!nikon_he::parse_picture_header(strip, length, ph) ||
        !nikon_he::is_supported_picture_header(ph, length) ||
        !ph.has_vendor_tail || ph.precinct_offset >= length)
        fail("Unsupported or invalid Nikon HE / HE* picture header.");
    if (ph.hdr_width != width || ph.hdr_height != height)
        fail("Nikon HE picture dimensions do not match the full raw sensor dimensions.");

    const int total_tiles = (height + 63) / 64;
    const int output_first = static_cast<int>(roi[1] / 64);
    const int output_last = static_cast<int>((roi[1] + roi[3] - 1) / 64);
    // step2 needs adjacent step1 rows; step1 needs adjacent coefficient rows.
    // Whole-tile halos are deliberately conservative and keep real edge flags.
    const int step1_first = (std::max)(0, output_first - 1);
    const int step1_last = (std::min)(total_tiles - 1, output_last + 1);
    const int coeff_first = (std::max)(0, step1_first - 1);
    const int coeff_last = (std::min)(total_tiles - 1, step1_last + 1);
    const int decode_first = coeff_first > 0 ? coeff_first - 1 : 0;

    const auto refs = index_precincts(strip, length, ph.precinct_offset, total_tiles);
    nikon_he::SubbandConfig config[26];
    nikon_he::LayoutInfo layout;
    nikon_he::compute_subband_layout(width / 2, config, layout);
    const int stripe_ints = nikon_he::compute_buf_stripe_ints(&layout);
    const int kband = nikon_he::compute_kband(&layout);
    if (stripe_ints <= 0 || kband < width / 2 ||
        static_cast<size_t>(3) * kband + width / 2 > static_cast<size_t>(stripe_ints))
        fail("Invalid Nikon HE full-width coefficient layout.");

    const size_t tile_ints = checked_mul(nikon_he::kStripesPerTile, stripe_ints);
    const size_t coeff_ints = checked_mul(coeff_last - coeff_first + 1, tile_ints);
    const size_t scratch_ints = checked_mul(step1_last - step1_first + 1, tile_ints);
    const size_t roi_pixels = checked_mul(roi[2], roi[3]);
    const size_t staging_pixels = checked_mul(64, width);

    // Bound owned buffers plus a conservative allowance for the existing
    // decode_tile implementation's transient vectors, predecessor and tables.
    size_t working_bytes = checked_mul(checked_add(coeff_ints, scratch_ints), 4);
    working_bytes = checked_add(working_bytes, checked_mul(roi_pixels, 2));
    working_bytes = checked_add(working_bytes, checked_mul(staging_pixels, 2));
    working_bytes = checked_add(working_bytes, checked_mul(2 * stripe_ints, 4));
    working_bytes = checked_add(working_bytes, checked_mul(nikon_he::kIqxIqpLutSize, 4));
    const size_t transient_ints = checked_add(
        checked_mul(50, stripe_ints), checked_add(checked_mul(16, kband), 16));
    working_bytes = checked_add(working_bytes, checked_mul(transient_ints, 4));
    working_bytes = checked_add(working_bytes, checked_mul(refs.capacity(), sizeof(PrecinctRef)));
    working_bytes = checked_add(working_bytes, 1024 * 1024);
    if (working_bytes >= 128u * 1024u * 1024u)
        fail("Nikon HE stripe request exceeds the experimental 128 MiB workspace budget.");

    result.total_stripes = total_tiles;
    const size_t first_precinct = checked_mul(decode_first, 16);
    const size_t end_precinct = (std::min)(refs.size(),
        checked_add(checked_mul(coeff_last, 16), nikon_he::kPrecinctsPerTile));
    for (size_t p = first_precinct; p < end_precinct; ++p)
        result.compressed_bytes_addressed = checked_add(
            result.compressed_bytes_addressed, checked_add(refs[p].payload_size, 12));
    result.header_index_ms = elapsed_ms(header_start);

    const auto allocation_start = Clock::now();
    std::vector<int32_t> coeff(coeff_ints, 0);
    std::vector<int32_t> scratch(scratch_ints, 0);
    std::vector<int32_t> overflow(checked_mul(2, stripe_ints), 0);
    std::vector<int32_t> transfer;
    make_transfer_lut(ph, transfer);
    std::vector<uint16_t> staging(staging_pixels, 0);
    result.pixels.resize(roi_pixels);
    result.allocation_ms = elapsed_ms(allocation_start);

    ActivePictureHeaderScope active_header(ph);
    const auto entropy_start = Clock::now();
    const uint8_t* predict_lut = nikon_he::build_prediction_lookup_table();
    for (int t = decode_first; t <= coeff_last; ++t) {
        std::array<const uint8_t*, nikon_he::kPrecinctsPerTile> pointers{};
        std::array<size_t, nikon_he::kPrecinctsPerTile> sizes{};
        const size_t base = static_cast<size_t>(t) * 16;
        // Match production's available-count behavior, including a missing
        // tile's count of zero. Never subtract base beyond the indexed range;
        // decode_tile itself rejects an invalid partial-tile tail state.
        const int count = base < refs.size() ? static_cast<int>((std::min)(
            static_cast<size_t>(nikon_he::kPrecinctsPerTile), refs.size() - base)) : 0;
        for (int p = 0; p < count; ++p) {
            pointers[p] = refs[base + p].data;
            sizes[p] = refs[base + p].payload_size;
        }
        PredecessorScope predecessor;
        predecessor.state.init(config); // Fresh PER TILE, exactly as in production.
        // Priming writes local slot zero, which the next tile overwrites.
        // Its possibly invalid first two coefficient rows are never consumed.
        const int local_tile = t < coeff_first ? 0 : t - coeff_first;
        const auto decoded = nikon_he::decode_tile(
            pointers.data(), sizes.data(), width, config, predecessor.state, predict_lut,
            coeff.data(), local_tile, overflow.data(),
            t == 0, t == total_tiles - 1, count);
        if (!decoded.success || decoded.precincts_decoded != count)
            fail("Nikon HE partial stripe entropy or inverse-wavelet decode failed.");
        ++result.decoded_stripes;
    }
    result.entropy_idwt_ms = elapsed_ms(entropy_start);

    const auto step1_start = Clock::now();
    for (int t = step1_first; t <= step1_last; ++t) {
        const bool last = t == total_tiles - 1;
        const int rows = last && height % 64 ? (height % 64) / 2
                                             : nikon_he::kStripesPerTile;
        const int32_t* c = coeff.data() + static_cast<size_t>(t - coeff_first) * tile_ints;
        int32_t* s = scratch.data() + static_cast<size_t>(t - step1_first) * tile_ints;
        nikon_he::step1_merge_4_to_2(
            c + kband, c, c + 3 * kband, c + 2 * kband,
            rows, width / 2, stripe_ints, stripe_ints,
            s, s + kband, stripe_ints, t == 0, last);
    }
    result.bayer_ms = elapsed_ms(step1_start);

    for (int t = output_first; t <= output_last; ++t) {
        const auto step2_start = Clock::now();
        const bool last = t == total_tiles - 1;
        const int rows = last && height % 64 ? (height % 64) / 2
                                             : nikon_he::kStripesPerTile;
        const int32_t* c = coeff.data() + static_cast<size_t>(t - coeff_first) * tile_ints;
        const int32_t* s = scratch.data() + static_cast<size_t>(t - step1_first) * tile_ints;
        // Only the output pointer's row origin is rebased. Transform width,
        // subband layout, coefficients and physical boundary flags are not.
        nikon_he::step2_bayer_rows(
            c + kband, s, s + kband, c + 2 * kband,
            rows, width / 2, stripe_ints, stripe_ints,
            transfer.data(), staging.data(), width, 0, t == 0, last);
        result.bayer_ms += elapsed_ms(step2_start);

        const auto copy_start = Clock::now();
        const unsigned tile_y = static_cast<unsigned>(t * 64);
        const unsigned begin_y = (std::max)(roi[1], tile_y);
        const unsigned end_y = (std::min)(roi[1] + roi[3],
                                         (std::min)(sensor_h, tile_y + 64));
        for (unsigned y = begin_y; y < end_y; ++y) {
            const uint16_t* source = staging.data()
                + static_cast<size_t>(y - tile_y) * width + roi[0];
            uint16_t* destination = result.pixels.data()
                + static_cast<size_t>(y - roi[1]) * roi[2];
            std::memcpy(destination, source, static_cast<size_t>(roi[2]) * sizeof(uint16_t));
        }
        result.copy_ms += elapsed_ms(copy_start);
    }
    return result;
}

} // namespace

NefStripeResult decode_nef_stripes(
    const uint8_t* strip, size_t length, int width, int height,
    const std::array<unsigned, 4>& roi) {
    const auto total_start = Clock::now();
    try {
        NefStripeResult result = decode_impl(strip, length, width, height, roi);
        // decode_impl's temporary buffers have been released at this point.
        result.total_ms = elapsed_ms(total_start);
        return result;
    } catch (const std::bad_alloc&) {
        fail("Unable to allocate the Nikon HE partial stripe workspace.");
    } catch (const std::length_error&) {
        fail("Nikon HE partial stripe buffer length is not representable.");
    }
}
