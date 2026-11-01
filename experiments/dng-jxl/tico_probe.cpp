// Recognition-only probe for the existing, source-established Windows x64 ABI.
// No image objects, private decode calls, codec keys, or input writes are used.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <bcrypt.h>
#include <io.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <iomanip>
#include <iostream>
#include <iterator>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#if !defined(_MSC_VER) || !defined(_M_X64) || !defined(_DLL) || defined(_DEBUG)
#error Build with Windows MSVC x64 Release /MD, not a debug or static CRT.
#endif
#if _ITERATOR_DEBUG_LEVEL != 0
#error The source-established foreign vector ABI requires _ITERATOR_DEBUG_LEVEL=0.
#endif

namespace fs = std::filesystem;
using Clock = std::chrono::steady_clock;
static_assert(sizeof(void*) == 8, "x64 only");
static_assert(sizeof(std::vector<unsigned char>) == 24, "MSVC Release vector ABI");
static_assert(alignof(std::vector<unsigned char>) == 8, "MSVC Release vector alignment");

namespace {
constexpr std::uint64_t kMaxFileBytes = 256ull * 1024 * 1024;
constexpr char kFrameworkHash[] = "f3064b4bf321402e6cce16388e135bb6b15de824ed717a6074a53784c9c662c0";
constexpr char kManifestHash[] = "59aa78cce27ee5a0b2ac36d767e75da4f5083e6431f2e422f1e060fa0dc4321b";
constexpr char kFileVersion[] = "26.13.0.260928112933";
constexpr char kRecognizeExport[] = "?IsIpxData@@YA_NAEBV?$vector@EV?$allocator@E@std@@@std@@@Z";

struct RuntimePin {
    const wchar_t* name;
    std::uint64_t bytes;
    const char* sha256;
};
// These exact entries are copied from the separately hash-pinned local manifest.
constexpr RuntimePin kRuntimePins[] = {
    {L"manifest.json", 2510, kManifestHash},
    {L"DxOFramework.dll", 142098568, kFrameworkHash},
    {L"raw_r.dll", 1468040, "00d88cd5aadd44722df9c774fd878d4bd3d41b262384c01acb2314bad41b574a"},
    {L"concrt140.dll", 324208, "2405355f0a58067b258f8df33c327e3a3d716eaac5a3a5aebb757842d85bd376"},
    {L"heif.dll", 918664, "f1325153433019dcd6527d6d059ff6ca0609a4ae39ae8993fad16fef3648cf27"},
    {L"msvcp140.dll", 557728, "0f885b509a685d2bbfa652fed26b5fb31d88fbdab0a978c641d1c7b8aa460aa9"},
    {L"msvcp140_atomic_wait.dll", 50304, "640b2aefced484d0368eea5bdd06addd0658a3a70a49256e560d6923b404a479"},
    {L"vcruntime140.dll", 124544, "d5e4d9a3e835fa679450145d6a7d94e36573a509317111904d9b3712c30d9066"},
    {L"vcruntime140_1.dll", 49792, "1f2d41c4aa5db0bc33ebf7b66d72943a817d7ce6cbe880502a9403823633093f"}
};

double elapsed(Clock::time_point start) {
    return std::chrono::duration<double>(Clock::now() - start).count();
}
std::runtime_error win_error(const char* operation) {
    return std::runtime_error(std::string(operation) + " failed (Win32 " + std::to_string(GetLastError()) + ")");
}
std::string utf8(const std::wstring& value) {
    if (value.empty()) return {};
    const int length = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
        static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
    if (!length) throw win_error("Unicode conversion");
    std::string result(static_cast<size_t>(length), '\0');
    if (!WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
        static_cast<int>(value.size()), result.data(), length, nullptr, nullptr))
        throw win_error("Unicode conversion");
    return result;
}
std::string quote(const std::string& value) {
    std::ostringstream out;
    out << '"';
    for (unsigned char byte : value) {
        if (byte == '"' || byte == '\\') out << '\\' << byte;
        else if (byte < 32) out << "\\u00" << std::hex << std::setw(2)
            << std::setfill('0') << static_cast<unsigned>(byte) << std::dec;
        else out << byte;
    }
    out << '"';
    return out.str();
}
const char* boolean(bool value) { return value ? "true" : "false"; }
void number(std::ostream& out, const std::optional<double>& value) {
    if (value) out << *value;
    else out << "null";
}

struct Handle {
    HANDLE value = INVALID_HANDLE_VALUE;
    explicit Handle(HANDLE handle = INVALID_HANDLE_VALUE) : value(handle) {}
    ~Handle() { if (value && value != INVALID_HANDLE_VALUE) CloseHandle(value); }
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
    Handle(Handle&& other) noexcept : value(std::exchange(other.value, INVALID_HANDLE_VALUE)) {}
};

struct Sha256 {
    BCRYPT_ALG_HANDLE algorithm = nullptr;
    BCRYPT_HASH_HANDLE hash = nullptr;
    static void check(NTSTATUS status) {
        if (status < 0) throw std::runtime_error("BCrypt SHA-256 operation failed");
    }
    Sha256() {
        check(BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0));
        const auto status = BCryptCreateHash(algorithm, &hash, nullptr, 0, nullptr, 0, 0);
        if (status < 0) {
            BCryptCloseAlgorithmProvider(algorithm, 0);
            algorithm = nullptr;
            check(status);
        }
    }
    ~Sha256() {
        if (hash) BCryptDestroyHash(hash);
        if (algorithm) BCryptCloseAlgorithmProvider(algorithm, 0);
    }
    Sha256(const Sha256&) = delete;
    Sha256& operator=(const Sha256&) = delete;
    void update(const unsigned char* data, size_t length) {
        while (length) {
            const auto count = static_cast<ULONG>(std::min<size_t>(length, 1024 * 1024));
            check(BCryptHashData(hash, const_cast<PUCHAR>(data), count, 0));
            data += count;
            length -= count;
        }
    }
    std::string finish() {
        unsigned char digest[32];
        check(BCryptFinishHash(hash, digest, sizeof(digest), 0));
        std::ostringstream out;
        for (unsigned char byte : digest) out << std::hex << std::setw(2)
            << std::setfill('0') << static_cast<unsigned>(byte);
        return out.str();
    }
};

struct LockedFile {
    Handle handle;
    std::uint64_t bytes = 0;
    explicit LockedFile(const fs::path& path) : handle(CreateFileW(path.c_str(), GENERIC_READ,
        FILE_SHARE_READ, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN, nullptr)) {
        if (handle.value == INVALID_HANDLE_VALUE) throw win_error("Opening and read-locking input");
        if (GetFileType(handle.value) != FILE_TYPE_DISK) throw std::runtime_error("Only disk files are accepted");
        LARGE_INTEGER length{};
        if (!GetFileSizeEx(handle.value, &length)) throw win_error("Reading file size");
        if (length.QuadPart <= 0 || static_cast<std::uint64_t>(length.QuadPart) > kMaxFileBytes)
            throw std::runtime_error("Input length must be between 1 and 268435456 bytes");
        bytes = static_cast<std::uint64_t>(length.QuadPart);
    }
    LockedFile(LockedFile&&) noexcept = default;
    LockedFile(const LockedFile&) = delete;
    LockedFile& operator=(const LockedFile&) = delete;
    void rewind() {
        LARGE_INTEGER zero{};
        if (!SetFilePointerEx(handle.value, zero, nullptr, FILE_BEGIN)) throw win_error("Seeking input");
    }
    void read_exact(unsigned char* destination, size_t length) {
        while (length) {
            const auto request = static_cast<DWORD>(std::min<size_t>(length, 1024 * 1024));
            DWORD got = 0;
            if (!ReadFile(handle.value, destination, request, &got, nullptr)) throw win_error("Reading input");
            if (!got) throw std::runtime_error("Unexpected input EOF");
            destination += got;
            length -= got;
        }
    }
    std::string sha256() {
        rewind();
        Sha256 hash;
        std::array<unsigned char, 65536> buffer{};
        auto remaining = bytes;
        while (remaining) {
            const auto count = static_cast<size_t>(std::min<std::uint64_t>(remaining, buffer.size()));
            read_exact(buffer.data(), count);
            hash.update(buffer.data(), count);
            remaining -= count;
        }
        return hash.finish();
    }
};

std::string file_version(const fs::path& path) {
    DWORD ignored = 0;
    const DWORD length = GetFileVersionInfoSizeW(path.c_str(), &ignored);
    if (!length || length > 1024 * 1024) throw std::runtime_error("Missing or oversized framework version resource");
    std::vector<unsigned char> data(length);
    if (!GetFileVersionInfoW(path.c_str(), 0, length, data.data())) throw win_error("Reading framework version");
    struct Translation { WORD language, codepage; };
    Translation* translations = nullptr;
    UINT translation_bytes = 0;
    if (!VerQueryValueW(data.data(), L"\\VarFileInfo\\Translation",
        reinterpret_cast<void**>(&translations), &translation_bytes))
        throw std::runtime_error("Missing framework version translation");
    for (UINT i = 0; i < translation_bytes / sizeof(Translation); ++i) {
        wchar_t key[80];
        swprintf_s(key, L"\\StringFileInfo\\%04x%04x\\FileVersion",
            static_cast<unsigned>(translations[i].language), static_cast<unsigned>(translations[i].codepage));
        wchar_t* text = nullptr;
        UINT characters = 0;
        if (VerQueryValueW(data.data(), key, reinterpret_cast<void**>(&text), &characters) && text && characters) {
            if (text[characters - 1] == L'\0') --characters;
            return utf8(std::wstring(text, characters));
        }
    }
    throw std::runtime_error("Missing framework FileVersion string");
}

fs::path module_path(HMODULE module) {
    std::wstring path(32768, L'\0');
    const DWORD length = GetModuleFileNameW(module, path.data(), static_cast<DWORD>(path.size()));
    if (!length || length >= path.size()) throw win_error("Reading loaded module path");
    path.resize(length);
    return fs::path(path);
}
struct Framework {
    HMODULE module = nullptr;
    // Exact signature from native/dxo_ipx_helper.cpp, Framework::Recognize.
    // Windows x64 uses one native calling convention; the vector is borrowed.
    using Recognize = bool (__cdecl*)(const std::vector<unsigned char>&);
    explicit Framework(const fs::path& path) {
        module = LoadLibraryExW(path.c_str(), nullptr,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
        if (!module) throw win_error("Framework LoadLibraryExW");
    }
    ~Framework() { if (module) FreeLibrary(module); }
    Framework(const Framework&) = delete;
    Framework& operator=(const Framework&) = delete;
    Recognize recognizer() const {
        auto result = GetProcAddress(module, kRecognizeExport);
        if (!result) throw std::runtime_error("Missing source-established IsIpxData export");
        return reinterpret_cast<Recognize>(result);
    }
};

struct Arguments { fs::path framework, payload; bool self_test = false; };
Arguments arguments(int argc, wchar_t** argv) {
    Arguments args;
    if (argc == 2 && std::wstring(argv[1]) == L"--self-test") {
        args.self_test = true;
        return args;
    }
    for (int i = 1; i < argc; i += 2) {
        if (i + 1 >= argc) throw std::runtime_error("Every option requires a value");
        const std::wstring name = argv[i];
        if (name == L"--framework" && args.framework.empty()) args.framework = argv[i + 1];
        else if (name == L"--payload" && args.payload.empty()) args.payload = argv[i + 1];
        else throw std::runtime_error("Unknown or repeated option: " + utf8(name));
    }
    if (!args.framework.is_absolute() || !args.payload.is_absolute())
        throw std::runtime_error("Required: --framework ABSOLUTE_DLL_PATH --payload ABSOLUTE_PAYLOAD_PATH");
    args.framework = fs::canonical(args.framework);
    args.payload = fs::canonical(args.payload);
    if (_wcsicmp(args.framework.filename().c_str(), L"DxOFramework.dll"))
        throw std::runtime_error("The pinned framework must retain its DxOFramework.dll filename");
    return args;
}

struct Report {
    bool self_test = false, completed = false, runtime_verified = false, dll_loaded = false;
    size_t runtime_files_verified = 0, payload_bytes = 0, payload_capacity = 0;
    std::string framework_path, payload_path, framework_hash, version, payload_hash, signature, error;
    std::string loaded_framework_path;
    std::optional<bool> recognized;
    std::optional<double> runtime_verify_seconds, version_seconds, payload_open_seconds;
    std::optional<double> reserve_seconds, first_touch_seconds, read_seconds, payload_hash_seconds;
    std::optional<double> load_seconds, post_load_setup_seconds, symbol_seconds, recognize_seconds;
    std::vector<std::pair<std::string, std::string>> loaded_dependencies;
};

std::string payload_signature(const std::vector<unsigned char>& payload) {
    constexpr unsigned char container[] = {0, 0, 0, 12, 'J', 'X', 'L', ' ', 13, 10, 135, 10};
    if (payload.size() >= 2 && payload[0] == 0xff && payload[1] == 0x0a) return "jpeg_xl_codestream_signature";
    if (payload.size() >= sizeof(container) && std::equal(std::begin(container), std::end(container), payload.begin()))
        return "jpeg_xl_container_signature";
    return "not_identified_by_signature_only";
}

void run(const Arguments& args, Report& report) {
    if (args.self_test) {
        report.self_test = true;
        Sha256 hash;
        hash.update(reinterpret_cast<const unsigned char*>("abc"), 3);
        if (hash.finish() != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            throw std::runtime_error("SHA-256 self-test failed");
        report.completed = true;
        return;
    }
    report.framework_path = utf8(args.framework.wstring());
    report.payload_path = utf8(args.payload.wstring());

    // Keep all locks until after Framework destruction. Deny writes/deletion.
    std::vector<LockedFile> runtime_locks;
    auto start = Clock::now();
    for (const auto& pin : kRuntimePins) {
        const fs::path path = _wcsicmp(pin.name, L"DxOFramework.dll") == 0
            ? args.framework : args.framework.parent_path() / pin.name;
        runtime_locks.emplace_back(path);
        auto& file = runtime_locks.back();
        if (file.bytes != pin.bytes || file.sha256() != pin.sha256)
            throw std::runtime_error("Pinned runtime size/SHA-256 mismatch: " + utf8(pin.name));
        ++report.runtime_files_verified;
    }
    report.runtime_verify_seconds = elapsed(start);
    report.runtime_verified = true;
    report.framework_hash = kFrameworkHash;
    start = Clock::now();
    report.version = file_version(args.framework);
    report.version_seconds = elapsed(start);
    if (report.version != kFileVersion) throw std::runtime_error("Pinned framework FileVersion mismatch");

    start = Clock::now();
    LockedFile source(args.payload);
    report.payload_open_seconds = elapsed(start);
    report.payload_bytes = static_cast<size_t>(source.bytes);
    // The established helper checks at least 32 bytes before calling IsIpxData.
    if (report.payload_bytes < 32) throw std::runtime_error("Payload is shorter than the established 32-byte probe minimum");
    std::vector<unsigned char> payload;
    start = Clock::now();
    payload.reserve(report.payload_bytes);
    report.reserve_seconds = elapsed(start);
    report.payload_capacity = payload.capacity();
    start = Clock::now();
    payload.resize(report.payload_bytes);
    report.first_touch_seconds = elapsed(start);
    start = Clock::now();
    source.read_exact(payload.data(), payload.size());
    report.read_seconds = elapsed(start);
    start = Clock::now();
    Sha256 hash;
    hash.update(payload.data(), payload.size());
    report.payload_hash = hash.finish();
    report.payload_hash_seconds = elapsed(start);
    report.signature = payload_signature(payload);

    start = Clock::now();
    Framework framework(args.framework);
    report.load_seconds = elapsed(start);
    report.dll_loaded = true;
    const auto setup_start = Clock::now();
    const auto loaded_framework = module_path(framework.module);
    report.loaded_framework_path = utf8(loaded_framework.wstring());
    if (!fs::equivalent(args.framework, loaded_framework)) {
        throw std::runtime_error("Framework path identity check failed");
    }
    // Record loader reuse of an already resident CRT instead of claiming that
    // every effective dependency necessarily came from the pinned directory.
    for (const auto& pin : kRuntimePins) {
        if (_wcsicmp(pin.name, L"manifest.json") == 0 || _wcsicmp(pin.name, L"DxOFramework.dll") == 0) continue;
        if (const auto module = GetModuleHandleW(pin.name))
            report.loaded_dependencies.emplace_back(utf8(pin.name), utf8(module_path(module).wstring()));
    }
    start = Clock::now();
    const auto recognize = framework.recognizer();
    report.symbol_seconds = elapsed(start);
    report.post_load_setup_seconds = elapsed(setup_start);
    start = Clock::now();
    report.recognized = recognize(payload);
    report.recognize_seconds = elapsed(start);
    report.completed = true;
    // Even an unexpected true is recognition only, never permission to guess a
    // decode ABI or construct a foreign destination for a different bitstream.
}

std::string json(const Report& r) {
    std::ostringstream out;
    out << std::setprecision(12) << "{\"schema\":1,\"program\":\"tico_probe\",\"probe_completed\":" << boolean(r.completed)
        << ",\"self_test\":" << boolean(r.self_test) << ",\"error\":" << (r.error.empty() ? "null" : quote(r.error))
        << ",\"status\":" << quote(!r.completed ? "probe_error" : r.self_test ? "self_test_passed" :
            r.recognized.value_or(false) ? "recognized_as_ipx_not_decoded" : "rejected_by_ipx_recognizer")
        << ",\"framework\":" << quote(r.framework_path) << ",\"framework_sha256\":" << quote(r.framework_hash)
        << ",\"expected_framework_sha256\":" << quote(kFrameworkHash)
        << ",\"manifest_sha256\":" << quote(kManifestHash) << ",\"runtime_manifest_version\":\"6.8.0.7\""
        << ",\"framework_file_version\":" << quote(r.version) << ",\"runtime_verified\":" << boolean(r.runtime_verified)
        << ",\"runtime_files_verified\":" << r.runtime_files_verified << ",\"dll_loaded\":" << boolean(r.dll_loaded)
        << ",\"loaded_framework\":" << quote(r.loaded_framework_path) << ",\"payload\":" << quote(r.payload_path)
        << ",\"payload_bytes\":" << r.payload_bytes << ",\"payload_capacity_bytes\":" << r.payload_capacity
        << ",\"payload_sha256\":" << quote(r.payload_hash) << ",\"payload_signature\":" << quote(r.signature)
        << ",\"recognizer_export\":" << quote(kRecognizeExport) << ",\"ipx_recognized\":";
    if (r.recognized) out << boolean(*r.recognized); else out << "null";
    out << ",\"native_decode_attempted\":false,\"native_decode_success\":null,\"decode_seconds\":null"
        << ",\"jpeg_xl_decode_tested\":false,\"msvc_full_version\":" << _MSC_FULL_VER
        << ",\"vector_abi_bytes\":" << sizeof(std::vector<unsigned char>) << ",\"iterator_debug_level\":0"
        << ",\"runtime_verify_seconds\":"; number(out, r.runtime_verify_seconds);
    out << ",\"framework_version_seconds\":"; number(out, r.version_seconds);
    out << ",\"payload_open_seconds\":"; number(out, r.payload_open_seconds);
    out << ",\"payload_reserve_seconds\":"; number(out, r.reserve_seconds);
    out << ",\"payload_first_touch_seconds\":"; number(out, r.first_touch_seconds);
    out << ",\"payload_read_seconds\":"; number(out, r.read_seconds);
    out << ",\"payload_hash_seconds\":"; number(out, r.payload_hash_seconds);
    out << ",\"framework_load_seconds\":"; number(out, r.load_seconds);
    out << ",\"post_load_setup_seconds\":"; number(out, r.post_load_setup_seconds);
    out << ",\"symbol_lookup_seconds\":"; number(out, r.symbol_seconds);
    out << ",\"recognize_seconds\":"; number(out, r.recognize_seconds);
    out << ",\"loaded_dependency_paths\":{";
    for (size_t i = 0; i < r.loaded_dependencies.size(); ++i) {
        if (i) out << ',';
        out << quote(r.loaded_dependencies[i].first) << ':' << quote(r.loaded_dependencies[i].second);
    }
    out << "},\"timing_scope\":\"ReadFile is buffered OS-cache I/O, not guaranteed physical disk I/O; reserve is vector capacity allocation; first touch is resize/zero-initialization with no cold-page guarantee; all verification, hashing, loading and symbol lookup are outside recognition timing.\""
        << ",\"result_scope\":\"Recognition-only program: the only eligible vendor call is the source-established Nikon IPX recognizer. Recognition is not decoding or proof of general JPEG XL support. No output pixels were allocated.\"}\n";
    return out.str();
}

Handle json_stdout() {
    HANDLE copy = INVALID_HANDLE_VALUE;
    if (!DuplicateHandle(GetCurrentProcess(), GetStdHandle(STD_OUTPUT_HANDLE), GetCurrentProcess(),
        &copy, 0, FALSE, DUPLICATE_SAME_ACCESS)) throw win_error("Preserving JSON stdout");
    Handle result(copy);
    std::cout.flush();
    std::fflush(stdout);
    // Match the established helper: vendor Win32 and CRT stdout go to stderr.
    if (!SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE)) ||
        _dup2(_fileno(stderr), _fileno(stdout)) != 0)
        throw std::runtime_error("Cannot redirect native diagnostics away from JSON stdout");
    return result;
}
void write_json(HANDLE output, const std::string& text) {
    size_t offset = 0;
    while (offset < text.size()) {
        const auto count = static_cast<DWORD>(std::min<size_t>(text.size() - offset, 65536));
        DWORD written = 0;
        if (!WriteFile(output, text.data() + offset, count, &written, nullptr) || !written)
            throw win_error("Writing JSON stdout");
        offset += written;
    }
}
} // namespace

int wmain(int argc, wchar_t** argv) {
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    try {
        auto output = json_stdout();
        Report report;
        try {
            if (!SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)) throw win_error("Restricting DLL search");
            run(arguments(argc, argv), report);
        } catch (const std::exception& error) { report.error = error.what(); }
          catch (...) { report.error = "Unknown native C++ exception; no decode was attempted"; }
        write_json(output.value, json(report));
        return report.completed ? 0 : 2;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 3;
    }
}
