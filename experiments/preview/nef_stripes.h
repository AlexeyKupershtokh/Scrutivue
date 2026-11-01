#ifndef SCRUTIVUE_EXPERIMENTS_NEF_STRIPES_H
#define SCRUTIVUE_EXPERIMENTS_NEF_STRIPES_H

#include <array>
#include <cstddef>
#include <cstdint>
#include <vector>

struct NefStripeResult {
    std::vector<uint16_t> pixels;
    // Counts 64-sensor-row decoder tiles, including the halo and priming tile.
    size_t decoded_stripes = 0;
    size_t total_stripes = 0;
    // Unique compressed precinct spans (12-byte prefix included), not disk I/O.
    // Excludes picture/slice headers and the header-only indexing walk.
    size_t compressed_bytes_addressed = 0;
    double header_index_ms = 0;
    double allocation_ms = 0;
    double entropy_idwt_ms = 0;
    double bayer_ms = 0;
    double copy_ms = 0;
    double total_ms = 0;
};

// Experimental source-level ROI decoding, NOT a public LibRaw ROI API.
// strip/length describe the COMPLETE compressed TIFF strip, including its
// picture header. width/height must be the original raw sensor dimensions.
// roi is {x, y, width, height} in uncropped raw coordinates. The result is a
// tightly packed row-major raw ROI; the source buffer is never modified.
// Throws std::runtime_error on unsupported input, invalid bounds or failure.
NefStripeResult decode_nef_stripes(
    const uint8_t* strip, size_t length, int width, int height,
    const std::array<unsigned, 4>& roi);

#endif
