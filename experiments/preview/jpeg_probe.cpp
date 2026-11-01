// Standalone libjpeg-turbo public-API experiment; not application code.
#define NOMINMAX
#include <windows.h>
#include <psapi.h>
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>
extern "C" {
#include <jpeglib.h>
}
#define PROBE_STRING_INNER(value) #value
#define PROBE_STRING(value) PROBE_STRING_INNER(value)

using Clock = std::chrono::steady_clock;
static double milliseconds(Clock::time_point a, Clock::time_point b) {
    return std::chrono::duration<double, std::milli>(b - a).count();
}
static PROCESS_MEMORY_COUNTERS_EX memory() {
    PROCESS_MEMORY_COUNTERS_EX m{};
    if (!GetProcessMemoryInfo(GetCurrentProcess(), reinterpret_cast<PROCESS_MEMORY_COUNTERS*>(&m), sizeof(m)))
        throw std::runtime_error("Cannot query process memory");
    return m;
}
static void print_memory(const char* name, const PROCESS_MEMORY_COUNTERS_EX& m) {
    std::cout << '"' << name << "\":{\"working_set_bytes\":" << m.WorkingSetSize
              << ",\"private_commit_bytes\":" << m.PrivateUsage
              << ",\"peak_working_set_bytes\":" << m.PeakWorkingSetSize
              << ",\"peak_private_commit_bytes\":" << m.PeakPagefileUsage << '}';
}
static unsigned number(const wchar_t* value) { return static_cast<unsigned>(std::stoul(value)); }

int wmain(int argc, wchar_t** argv) {
    try {
        if (argc != 4 && argc != 5 && argc != 8 && argc != 9) {
            std::cerr << "Usage: jpeg_probe INPUT DENOM full [OUTPUT] | INPUT DENOM roi X Y W H [OUTPUT]\n";
            return 2;
        }
        const bool roi = std::wstring(argv[3]) == L"roi";
        if ((roi && argc < 8) || (!roi && std::wstring(argv[3]) != L"full"))
            throw std::runtime_error("Invalid decode mode");
        const unsigned denom = number(argv[2]);
        if (denom != 1 && denom != 2 && denom != 4 && denom != 8)
            throw std::runtime_error("Only denominators 1, 2, 4 and 8 are supported by this probe");
        const auto process_before = memory();
        const auto input_begin = Clock::now();
        std::ifstream file(std::filesystem::path(argv[1]), std::ios::binary | std::ios::ate);
        if (!file) throw std::runtime_error("Cannot open JPEG input");
        const auto length = file.tellg();
        if (length <= 0 || static_cast<uint64_t>(length) > 512ull * 1024 * 1024)
            throw std::runtime_error("Input exceeds the 512 MiB experiment bound or is empty");
        const auto input_open_end = Clock::now();
        std::vector<unsigned char> encoded(static_cast<size_t>(length));
        const auto input_alloc_end = Clock::now();
        const auto input_allocated = memory();
        const auto input_read_begin = Clock::now();
        file.seekg(0);
        file.read(reinterpret_cast<char*>(encoded.data()), length);
        if (!file) throw std::runtime_error("Cannot read JPEG input");
        const auto input_read_end = Clock::now();
        const auto before = memory();
        const auto begin = Clock::now();
        jpeg_decompress_struct c{};
        jpeg_error_mgr errors{};
        c.err = jpeg_std_error(&errors);
        // The default fatal handler exits this isolated worker on corrupt input.
        jpeg_create_decompress(&c);
        jpeg_mem_src(&c, encoded.data(), static_cast<unsigned long>(encoded.size()));
        jpeg_read_header(&c, TRUE);
        const unsigned source_width = c.image_width, source_height = c.image_height;
        const bool progressive = c.progressive_mode != 0;
        c.scale_num = 1;
        c.scale_denom = denom;
        c.out_color_space = JCS_RGB;
        c.dct_method = JDCT_ISLOW;
        c.do_fancy_upsampling = TRUE;
        const auto header_end = Clock::now();
        const auto header_memory = memory();
        const auto start_begin = Clock::now();
        jpeg_start_decompress(&c);
        const auto start_end = Clock::now();
        const auto start_memory = memory();
        const auto crop_begin = Clock::now();
        const unsigned scaled_width = c.output_width, scaled_height = c.output_height;
        unsigned x = 0, y = 0, width = scaled_width, height = scaled_height;
        if (roi) {
            x = number(argv[4]); y = number(argv[5]);
            width = number(argv[6]); height = number(argv[7]);
        }
        if (!width || !height || static_cast<uint64_t>(x) + width > scaled_width ||
            static_cast<uint64_t>(y) + height > scaled_height)
            throw std::runtime_error("ROI is outside the scaled image");
        JDIMENSION aligned_x = x, aligned_width = width;
        if (roi) jpeg_crop_scanline(&c, &aligned_x, &aligned_width);
        const auto crop_end = Clock::now();
        const auto crop_memory = memory();
        const auto skip_begin = Clock::now();
        if (roi && jpeg_skip_scanlines(&c, y) != y)
            throw std::runtime_error("Decoder did not skip the requested rows");
        const auto skip_end = Clock::now();
        const auto skip_memory = memory();
        const auto allocation_begin = Clock::now();
        std::vector<unsigned char> pixels(static_cast<size_t>(width) * height * 3);
        std::vector<unsigned char> row(roi ? static_cast<size_t>(aligned_width) * 3 : 0);
        const auto allocation_end = Clock::now();
        const auto allocated_memory = memory();
        const auto scanlines_begin = Clock::now();
        for (unsigned n = 0; n < height; ++n) {
            unsigned char* destination = pixels.data() + static_cast<size_t>(n) * width * 3;
            JSAMPROW line = roi ? row.data() : destination;
            if (jpeg_read_scanlines(&c, &line, 1) != 1)
                throw std::runtime_error("Decoder produced no output row");
            if (roi) std::memcpy(destination, row.data() + static_cast<size_t>(x - aligned_x) * 3,
                                 static_cast<size_t>(width) * 3);
        }
        const auto output_end = Clock::now();
        const size_t consumed = encoded.size() - c.src->bytes_in_buffer;
        const auto after = memory();
        // ROI intentionally stops at the requested bottom edge, not image EOF.
        const auto cleanup_begin = Clock::now();
        jpeg_abort_decompress(&c);
        jpeg_destroy_decompress(&c);
        const auto cleanup_end = Clock::now();
        const auto cleanup_memory = memory();
        const wchar_t* output = roi ? (argc == 9 ? argv[8] : nullptr) : (argc == 5 ? argv[4] : nullptr);
        const auto dump_begin = Clock::now();
        if (output) {
            std::ofstream dump(std::filesystem::path(output), std::ios::binary);
            dump.write(reinterpret_cast<const char*>(pixels.data()), pixels.size());
            dump.close();
            if (!dump) throw std::runtime_error("Cannot write RGB output");
        }
        const auto dump_end = Clock::now();
        std::cout << "{\"probe_schema_version\":2,\"backend\":\"libjpeg-turbo " << PROBE_STRING(LIBJPEG_TURBO_VERSION)
                  << " scalar\",\"denominator\":" << denom
                  << ",\"progressive\":" << (progressive ? "true" : "false")
                  << ",\"source_width\":" << source_width << ",\"source_height\":" << source_height
                  << ",\"scaled_width\":" << scaled_width << ",\"scaled_height\":" << scaled_height
                  << ",\"x\":" << x << ",\"y\":" << y << ",\"width\":" << width << ",\"height\":" << height
                  << ",\"aligned_x\":" << aligned_x << ",\"aligned_width\":" << aligned_width
                  << ",\"encoded_bytes\":" << encoded.size() << ",\"consumed_bytes\":" << consumed
                  << ",\"output_bytes\":" << pixels.size()
                  << ",\"row_scratch_bytes\":" << row.size()
                  << ",\"input_open_ms\":" << milliseconds(input_begin, input_open_end)
                  << ",\"input_allocation_ms\":" << milliseconds(input_open_end, input_alloc_end)
                  << ",\"input_read_ms\":" << milliseconds(input_read_begin, input_read_end)
                  << ",\"header_ms\":" << milliseconds(begin, header_end)
                  << ",\"start_ms\":" << milliseconds(start_begin, start_end)
                  << ",\"crop_ms\":" << milliseconds(crop_begin, crop_end)
                  << ",\"skip_ms\":" << milliseconds(skip_begin, skip_end)
                  << ",\"output_allocation_ms\":" << milliseconds(allocation_begin, allocation_end)
                  << ",\"scanlines_ms\":" << milliseconds(scanlines_begin, output_end)
                  << ",\"output_ms\":" << milliseconds(allocation_begin, allocation_end) + milliseconds(scanlines_begin, output_end)
                  << ",\"decode_ms\":" << milliseconds(begin, output_end)
                  << ",\"decode_stage_sum_ms\":" << milliseconds(begin, header_end) + milliseconds(start_begin, start_end)
                      + milliseconds(crop_begin, crop_end) + milliseconds(skip_begin, skip_end)
                      + milliseconds(allocation_begin, allocation_end) + milliseconds(scanlines_begin, output_end)
                  << ",\"cleanup_ms\":" << milliseconds(cleanup_begin, cleanup_end)
                  << ",\"dump_write_ms\":" << (output ? milliseconds(dump_begin, dump_end) : 0.0)
                  << ",\"working_set_before_bytes\":" << before.WorkingSetSize
                  << ",\"peak_working_set_bytes\":" << after.PeakWorkingSetSize
                  << ",\"peak_private_commit_bytes\":" << after.PeakPagefileUsage
                  << ",\"memory\":{";
        print_memory("process_before_input", process_before);
        std::cout << ','; print_memory("input_allocated", input_allocated);
        std::cout << ','; print_memory("input_read", before);
        std::cout << ','; print_memory("header", header_memory);
        std::cout << ','; print_memory("start_decompress", start_memory);
        std::cout << ','; print_memory("crop", crop_memory);
        std::cout << ','; print_memory("skip", skip_memory);
        std::cout << ','; print_memory("output_allocated", allocated_memory);
        std::cout << ','; print_memory("output_decoded", after);
        std::cout << ','; print_memory("decoder_destroyed_buffers_retained", cleanup_memory);
        std::cout << "}}\n";
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
    return 0;
}
