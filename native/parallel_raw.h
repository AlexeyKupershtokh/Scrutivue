// Parallel adapters for the pinned LibRaw implementation; vendor sources stay unchanged.
// Same alternative LGPL-2.1 / CDDL-1.0 terms as parallel_raw.cpp; see its attribution.
#pragma once
#include "libraw/libraw.h"
#include <cstddef>
#include <cstdint>

class ParallelRaw final : public LibRaw {
public:
    using Cancel = int (*)(void *);
    bool enable_parallel(const void *bytes, size_t length, Cancel cancel, void *context);
    bool parallel_enabled() const { return enabled_; }
    int get_decoder_info(libraw_decoder_info_t *info) override;
    libraw_processed_image_t *dcraw_make_mem_image(int *error = nullptr) override;
    static void test_kernels();

protected:
    void copy_bayer(unsigned short black[4], unsigned short *maximum) override;
    void scale_colors_loop(float multipliers[4]) override;
    void lin_interpolate_loop(int *code, int size) override;
    void convert_to_rgb_loop(float matrix[3][4]) override;

private:
    void load_nef_parallel();
    void check_cancelled() const;
    bool enabled_ = false;
    const uint8_t *strip_ = nullptr;
    size_t strip_size_ = 0;
    Cancel cancel_ = nullptr;
    void *cancel_context_ = nullptr;
    void (LibRaw::*original_loader_)() = nullptr;
};
