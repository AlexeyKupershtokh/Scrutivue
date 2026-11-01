// Isolated probe of the exact production LibRaw archive; no thumbnail API is used.
#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <windows.h>
#include <psapi.h>
#include "libraw/libraw.h"
#include "nef_stripes.h"
#include "../../vendor/libraw/src/decoders/nikon_he/nikon_he_picture_header.h"
#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

using Clock = std::chrono::steady_clock;
static double ms(Clock::time_point start) {
    return std::chrono::duration<double, std::milli>(Clock::now() - start).count();
}
static void check(bool condition, const char* message) { if (!condition) throw std::runtime_error(message); }
static void libcheck(int status) { if (status) throw std::runtime_error(libraw_strerror(status)); }
static std::string quote(const std::string& value) {
    std::ostringstream out;
    out << '"';
    for (unsigned char c : value) {
        if (c == '"' || c == '\\') out << '\\' << c;
        else if (c < 32) out << "\\u" << std::hex << std::setw(4) << std::setfill('0') << unsigned(c) << std::dec;
        else out << c;
    }
    out << '"';
    return out.str();
}
struct Report {
    std::ostringstream out;
    Report() { out << std::setprecision(12) << "{\"success\":true"; }
    void number(const char* key, double value) { out << ',' << quote(key) << ':' << value; }
    void text(const char* key, const std::string& value) { out << ',' << quote(key) << ':' << quote(value); }
    void memory(const char* key) {
        PROCESS_MEMORY_COUNTERS_EX p{}; p.cb = sizeof(p);
        check(GetProcessMemoryInfo(GetCurrentProcess(), reinterpret_cast<PROCESS_MEMORY_COUNTERS*>(&p), sizeof(p)) != 0, "Read process memory counters failed");
        out << ',' << quote(key) << ":{\"peak_ws_bytes\":" << p.PeakWorkingSetSize << ",\"ws_bytes\":" << p.WorkingSetSize
            << ",\"private_bytes\":" << p.PrivateUsage << ",\"page_faults\":" << p.PageFaultCount << '}';
    }
};
struct Mapping {
    HANDLE file = INVALID_HANDLE_VALUE, mapping = nullptr;
    const uint8_t* data = nullptr;
    size_t size = 0;
    explicit Mapping(const std::filesystem::path& path) {
        file = CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        check(file != INVALID_HANDLE_VALUE, "Open read-only source failed");
        LARGE_INTEGER length{};
        check(GetFileSizeEx(file, &length) && length.QuadPart > 0 && length.QuadPart <= 2LL * 1024 * 1024 * 1024, "Input outside 2 GiB bound");
        size = size_t(length.QuadPart);
        mapping = CreateFileMappingW(file, nullptr, PAGE_READONLY, 0, 0, nullptr);
        check(mapping != nullptr, "Create read-only mapping failed");
        data = static_cast<const uint8_t*>(MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0));
        check(data != nullptr, "Map read-only input failed");
    }
    ~Mapping() { if (data) UnmapViewOfFile(data); if (mapping) CloseHandle(mapping); if (file != INVALID_HANDLE_VALUE) CloseHandle(file); }
};
struct ImageDeleter { void operator()(libraw_processed_image_t* image) const { if (image) LibRaw::dcraw_clear_mem(image); } };
using Image = std::unique_ptr<libraw_processed_image_t, ImageDeleter>;
static Image process(LibRaw& decoder, Report& report, const char* process_key, const char* bitmap_key) {
    auto timer = Clock::now();
    libcheck(decoder.dcraw_process());
    report.number(process_key, ms(timer));
    timer = Clock::now();
    int status = 0;
    Image image(decoder.dcraw_make_mem_image(&status));
    libcheck(status);
    check(image && image->bits == 16 && image->colors == 3 && image->type == LIBRAW_IMAGE_BITMAP, "Unexpected developed output layout");
    report.number(bitmap_key, ms(timer));
    return image;
}
static void write_new(const std::filesystem::path& path, const uint16_t* pixels, size_t count) {
    HANDLE file = CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
    check(file != INVALID_HANDLE_VALUE, "Create new output failed (existing outputs are never replaced)");
    DWORD written = 0;
    const bool ok = count <= MAXDWORD / 2 && WriteFile(file, pixels, DWORD(count * 2), &written, nullptr) && written == count * 2;
    CloseHandle(file);
    check(ok, "Write output failed");
}
static void compare(Report& report, const uint16_t* pixels, size_t width, size_t height, size_t channels,
                    const std::filesystem::path& path, size_t full_width, size_t full_height, size_t left, size_t top) {
    auto timer = Clock::now();
    Mapping reference(path);
    check(reference.size == full_width * full_height * channels * 2, "Reference output length mismatch");
    check(left + width <= full_width && top + height <= full_height, "Reference crop outside image");
    const auto* expected = reinterpret_cast<const uint16_t*>(reference.data);
    uint64_t different = 0, sum = 0;
    unsigned maximum = 0;
    for (size_t y = 0; y < height; ++y) for (size_t x = 0; x < width * channels; ++x) {
        const int actual = pixels[(y * width * channels) + x];
        const int other = expected[((top + y) * full_width + left) * channels + x];
        const unsigned error = unsigned(std::abs(actual - other));
        different += error != 0; sum += error; maximum = std::max(maximum, error);
    }
    report.number("different_samples", double(different));
    report.number("maximum_absolute_error", maximum);
    report.number("mean_absolute_error", double(sum) / (width * height * channels));
    report.number("validation_ms", ms(timer));
}

int wmain(int argc, wchar_t** argv) {
    try {
        check(argc >= 3, "Usage: nef_libraw.exe MODE INPUT [--roi x y w h] [--halo N] [--fixed-maximum] [--output NEW_U16] [--reference U16]");
        const std::wstring mode = argv[1];
        std::string mode_name;
        for (wchar_t c : mode) { check(c >= 0 && c < 128, "Mode must be ASCII"); mode_name.push_back(static_cast<char>(c)); }
        std::filesystem::path source = argv[2], output, reference;
        std::array<unsigned, 4> roi{};
        bool have_roi = false, fixed_maximum = false;
        unsigned halo = 0;
        for (int i = 3; i < argc; ++i) {
            const std::wstring key = argv[i];
            if (key == L"--fixed-maximum") fixed_maximum = true;
            else if (key == L"--roi") {
                check(i + 4 < argc, "Missing ROI coordinates");
                for (unsigned& coordinate : roi) coordinate = unsigned(std::stoul(argv[++i]));
                have_roi = true;
            } else if (key == L"--halo") { check(i + 1 < argc, "Missing halo"); halo = unsigned(std::stoul(argv[++i])); }
            else if (key == L"--output") { check(i + 1 < argc, "Missing output path"); output = argv[++i]; }
            else if (key == L"--reference") { check(i + 1 < argc, "Missing reference path"); reference = argv[++i]; }
            else throw std::runtime_error("Unknown argument");
        }
        check(mode == L"metadata" || mode == L"raw-full" || mode == L"full" || mode == L"half" || mode == L"quarter" || mode == L"roi" || mode == L"half-roi" || mode == L"resume" || mode == L"stripe-roi" || mode == L"stripe-self-test", "Unknown probe mode");
        Report report; report.text("mode", mode_name);
        report.text("libraw_commit", "f4956c404fba0c6035ce9e06353d4b4ffe068fc7");
        report.text("io_scope", "read-only mmap; no prefault; demand faults are in metadata/unpack/stripe work; OS cache not flushed");
        auto total = Clock::now();
        auto timer = Clock::now();
        Mapping mapping(source);
        report.number("source_bytes", double(mapping.size)); report.number("open_map_ms", ms(timer));
        timer = Clock::now();
        auto decoder = std::make_unique<LibRaw>();
        report.number("constructor_ms", ms(timer));
        auto& p = decoder->imgdata.params;
        p.output_bps = 16; p.output_color = 1; p.use_camera_wb = 1; p.no_auto_bright = 1;
        p.user_qual = 0; p.med_passes = 0; p.user_flip = 0;
        if (fixed_maximum) p.adjust_maximum_thr = 0;
        timer = Clock::now();
        libcheck(decoder->open_buffer(mapping.data, mapping.size));
        report.number("metadata_ms", ms(timer));
        const auto sizes = decoder->imgdata.sizes;
        const unsigned raw_width = sizes.raw_width, raw_height = sizes.raw_height;
        const unsigned width = sizes.width, height = sizes.height;
        report.text("make", decoder->imgdata.idata.make); report.text("model", decoder->imgdata.idata.model);
        report.text("decoder", decoder->unpack_function_name());
        report.number("nef_compression", decoder->imgdata.makernotes.nikon.NEFCompression);
        report.number("raw_width", raw_width); report.number("raw_height", raw_height);
        report.number("active_width", width); report.number("active_height", height);
        report.number("left_margin", sizes.left_margin); report.number("top_margin", sizes.top_margin);
        report.number("stored_flip", sizes.flip); report.number("output_flip", 0);
        report.number("fixed_maximum", fixed_maximum);
        auto* internal = decoder->get_internal_data_pointer();
        const auto offset = internal->unpacker_data.data_offset, length = internal->unpacker_data.data_size;
        report.number("strip_offset", double(offset)); report.number("strip_bytes", double(length));
        if (mode == L"metadata") { report.out << '}'; std::cout << report.out.str() << '\n'; return 0; }
        if (mode == L"stripe-self-test") {
            check(offset >= 0 && length >= 64 && uint64_t(offset) <= mapping.size && uint64_t(length) <= mapping.size - uint64_t(offset), "Invalid NEF compressed strip bounds");
            const uint8_t* strip = mapping.data + size_t(offset);
            const std::array<unsigned,4> good_roi{0,0,16,16};
            unsigned passed = 0;
            auto rejects = [&](const uint8_t* data, size_t size, int w, int h, std::array<unsigned,4> region) {
                bool rejected = false;
                try { auto unused = decode_nef_stripes(data, size, w, h, region); }
                catch (const std::exception&) { rejected = true; }
                check(rejected, "Invalid stripe input was not rejected"); ++passed;
            };
            rejects(strip, size_t(length), raw_width, raw_height, {0,0,0,1});
            rejects(strip, size_t(length), raw_width, raw_height, {(std::numeric_limits<unsigned>::max)(),0,1,1});
            rejects(strip, size_t(length), raw_width, raw_height, {raw_width-1,raw_height-1,2,2});
            rejects(strip, size_t(length), raw_width-8, raw_height, good_roi);
            rejects(strip, 32, raw_width, raw_height, good_roi);
            rejects(strip, size_t(length)/2, raw_width, raw_height, good_roi);
            nikon_he::PictureHeader ph;
            check(nikon_he::parse_picture_header(strip, size_t(length), ph) && ph.precinct_offset >= 6, "Self-test source picture header invalid");
            std::vector<uint8_t> damaged(strip, strip + size_t(length));
            damaged[ph.precinct_offset-6] ^= 1;
            rejects(damaged.data(), damaged.size(), raw_width, raw_height, good_roi);
            report.number("negative_tests_passed", passed);
            report.out << '}'; std::cout << report.out.str() << '\n'; return 0;
        }
        const bool raw_mode = mode == L"raw-full" || mode == L"stripe-roi";
        const unsigned full_width = raw_mode ? raw_width : width;
        const unsigned full_height = raw_mode ? raw_height : height;
        if (!have_roi) roi = {((full_width - 512) / 2) & ~1U, ((full_height - 512) / 2) & ~1U, 512, 512};
        check(roi[2] > 0 && roi[3] > 0 && uint64_t(roi[0]) + roi[2] <= full_width && uint64_t(roi[1]) + roi[3] <= full_height, "ROI outside image");
        for (unsigned i = 0; i < 4; ++i) report.number(("roi_" + std::to_string(i)).c_str(), roi[i]);
        std::vector<uint16_t> owned;
        Image image;
        const uint16_t* pixels = nullptr;
        unsigned out_width = full_width, out_height = full_height, channels = raw_mode ? 1 : 3;
        if (mode == L"stripe-roi") {
            check(decoder->imgdata.makernotes.nikon.NEFCompression == 13 || decoder->imgdata.makernotes.nikon.NEFCompression == 14, "Stripe experiment supports Nikon HE/HE* only");
            check(offset >= 0 && length >= 64 && uint64_t(offset) <= mapping.size && uint64_t(length) <= mapping.size - uint64_t(offset), "Invalid NEF compressed strip bounds");
            auto result = decode_nef_stripes(mapping.data + size_t(offset), size_t(length), int(raw_width), int(raw_height), roi);
            report.number("selected_stripes", double(result.decoded_stripes)); report.number("total_stripes", double(result.total_stripes));
            report.number("compressed_bytes_addressed", double(result.compressed_bytes_addressed));
            report.number("header_index_ms", result.header_index_ms); report.number("output_internal_alloc_ms", result.allocation_ms);
            report.number("entropy_idwt_ms", result.entropy_idwt_ms); report.number("bayer_ms", result.bayer_ms);
            report.number("roi_copy_ms", result.copy_ms); report.number("stripe_total_ms", result.total_ms);
            owned = std::move(result.pixels); pixels = owned.data(); out_width = roi[2]; out_height = roi[3];
            report.text("domain", "stored-orientation sensor CFA; custom source-level stripe orchestration, not a public LibRaw ROI API or developed RGB");
        } else {
            p.half_size = mode == L"half" || mode == L"quarter" || mode == L"half-roi" || mode == L"resume";
            timer = Clock::now(); libcheck(decoder->unpack()); report.number("unpack_ms", ms(timer));
            report.memory("after_unpack");
            if (mode == L"raw-full") {
                check(decoder->imgdata.rawdata.raw_image != nullptr, "Expected Bayer raw_image");
                timer = Clock::now(); owned.resize(size_t(raw_width) * raw_height); report.number("caller_output_alloc_ms", ms(timer));
                timer = Clock::now();
                const auto* raw = decoder->imgdata.rawdata.raw_image;
                const size_t pitch = decoder->imgdata.sizes.raw_pitch / 2;
                check(pitch >= raw_width, "Invalid raw row stride");
                for (unsigned y = 0; y < raw_height; ++y) std::copy_n(raw + y * pitch, raw_width, owned.data() + size_t(y) * raw_width);
                report.number("raw_copy_ms", ms(timer)); pixels = owned.data();
            } else {
                const bool crop = mode == L"roi" || mode == L"half-roi";
                unsigned crop_x = roi[0], crop_y = roi[1], crop_width = roi[2], crop_height = roi[3];
                if (crop) {
                    crop_x = roi[0] > halo ? roi[0] - halo : 0; crop_y = roi[1] > halo ? roi[1] - halo : 0;
                    crop_x &= ~1U; crop_y &= ~1U;
                    crop_width = unsigned(std::min<uint64_t>(full_width, uint64_t(roi[0]) + roi[2] + halo)) - crop_x;
                    crop_height = unsigned(std::min<uint64_t>(full_height, uint64_t(roi[1]) + roi[3] + halo)) - crop_y;
                    p.cropbox[0] = crop_x; p.cropbox[1] = crop_y; p.cropbox[2] = crop_width; p.cropbox[3] = crop_height;
                    report.number("development_halo", halo);
                }
                image = process(*decoder, report, "process_ms", "bitmap_ms");
                if (mode == L"resume") {
                    report.number("half_ready_ms", ms(total)); report.memory("half_ready_memory");
                    report.number("half_output_bytes", image->data_size);
                    image.reset(); decoder->free_image(); p.half_size = 0;
                    timer = Clock::now();
                    image = process(*decoder, report, "resume_process_ms", "resume_bitmap_ms");
                    report.number("resume_full_ms", ms(timer));
                    report.text("reuse", "same LibRaw object and retained full raw samples; no second unpack");
                }
                out_width = image->width; out_height = image->height;
                pixels = reinterpret_cast<const uint16_t*>(image->data);
                if (crop) {
                    const unsigned scale = p.half_size ? 2 : 1;
                    check(roi[0] % scale == 0 && roi[1] % scale == 0 && roi[2] % scale == 0 && roi[3] % scale == 0, "Half ROI requires even coordinates and dimensions");
                    const unsigned dx = (roi[0] - crop_x) / scale, dy = (roi[1] - crop_y) / scale;
                    const unsigned rw = roi[2] / scale, rh = roi[3] / scale;
                    check(dx + rw <= out_width && dy + rh <= out_height, "Developed ROI output unexpectedly clipped");
                    timer = Clock::now(); owned.resize(size_t(rw) * rh * 3); report.number("caller_output_alloc_ms", ms(timer));
                    timer = Clock::now();
                    for (unsigned y = 0; y < rh; ++y) std::copy_n(pixels + ((size_t(dy + y) * out_width + dx) * 3), rw * 3, owned.data() + size_t(y) * rw * 3);
                    report.number("roi_copy_ms", ms(timer)); out_width = rw; out_height = rh; pixels = owned.data();
                } else if (mode == L"quarter") {
                    const unsigned qw = out_width / 2, qh = out_height / 2;
                    timer = Clock::now(); owned.resize(size_t(qw) * qh * 3); report.number("caller_output_alloc_ms", ms(timer));
                    timer = Clock::now();
                    for (unsigned y = 0; y < qh; ++y) for (unsigned x = 0; x < qw; ++x) for (unsigned c = 0; c < 3; ++c) {
                        const size_t first = (size_t(y * 2) * out_width + x * 2) * 3 + c;
                        const uint32_t sum = pixels[first] + pixels[first + 3] + pixels[first + size_t(out_width) * 3] + pixels[first + size_t(out_width) * 3 + 3];
                        owned[(size_t(y) * qw + x) * 3 + c] = uint16_t((sum + 2) / 4);
                    }
                    report.number("post_half_box_resize_ms", ms(timer)); out_width = qw; out_height = qh; pixels = owned.data();
                }
                report.text("domain", "unrotated sRGB16, camera WB, bilinear development, no auto brightness; no thumbnail");
            }
        }
        report.number("ready_ms", ms(total)); report.memory("ready_memory");
        report.number("output_width", out_width); report.number("output_height", out_height); report.number("channels", channels);
        report.number("output_bytes", double(size_t(out_width) * out_height * channels * 2));
        report.number("adjusted_maximum", decoder->imgdata.color.maximum);
        if (!reference.empty()) {
            unsigned rx = 0, ry = 0, reference_width = full_width, reference_height = full_height;
            if (mode == L"roi" || mode == L"stripe-roi") { rx = roi[0]; ry = roi[1]; }
            else if (mode == L"half-roi") { rx = roi[0] / 2; ry = roi[1] / 2; reference_width = full_width / 2; reference_height = full_height / 2; }
            compare(report, pixels, out_width, out_height, channels, reference, reference_width, reference_height, rx, ry);
        }
        if (!output.empty()) { timer = Clock::now(); write_new(output, pixels, size_t(out_width) * out_height * channels); report.number("export_ms", ms(timer)); }
        image.reset(); timer = Clock::now(); decoder.reset(); report.number("decoder_release_ms", ms(timer));
        report.out << '}'; std::cout << report.out.str() << '\n'; return 0;
    } catch (const std::exception& error) {
        std::cerr << "{\"success\":false,\"error\":" << quote(error.what()) << "}\n"; return 1;
    } catch (...) { std::cerr << "{\"success\":false,\"error\":\"Unknown native exception\"}\n"; return 1; }
}
