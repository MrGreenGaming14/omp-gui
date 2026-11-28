#include "loa/surfaces/docs/ooxml.hpp"

#include <algorithm>
#include <charconv>
#include <cstring>
#include <format>

#include <minizip/unzip.h>

namespace loa::surfaces::docs::ooxml {

namespace {

// A read-only minizip stream over borrowed bytes.
struct MemStream {
    std::span<const std::uint8_t> bytes;
    std::uint64_t pos = 0;
};

voidpf ZCALLBACK mem_open(voidpf opaque, const void*, int) { return opaque; }

uLong ZCALLBACK mem_read(voidpf, voidpf stream, void* buf, uLong size) {
    auto* s = static_cast<MemStream*>(stream);
    const std::uint64_t left = s->bytes.size() - std::min<std::uint64_t>(s->pos, s->bytes.size());
    const auto n = static_cast<uLong>(std::min<std::uint64_t>(size, left));
    if (n > 0) std::memcpy(buf, s->bytes.data() + s->pos, n);
    s->pos += n;
    return n;
}

uLong ZCALLBACK mem_write(voidpf, voidpf, const void*, uLong) { return 0; }

ZPOS64_T ZCALLBACK mem_tell(voidpf, voidpf stream) { return static_cast<MemStream*>(stream)->pos; }

long ZCALLBACK mem_seek(voidpf, voidpf stream, ZPOS64_T offset, int origin) {
    auto* s = static_cast<MemStream*>(stream);
    std::uint64_t base = 0;
    switch (origin) {
        case ZLIB_FILEFUNC_SEEK_SET: base = 0; break;
        case ZLIB_FILEFUNC_SEEK_CUR: base = s->pos; break;
        case ZLIB_FILEFUNC_SEEK_END: base = s->bytes.size(); break;
        default: return -1;
    }
    const std::uint64_t target = base + offset;
    if (target > s->bytes.size()) return -1;
    s->pos = target;
    return 0;
}

int ZCALLBACK mem_close(voidpf, voidpf) { return 0; }
int ZCALLBACK mem_error(voidpf, voidpf) { return 0; }

// Whether `s` is valid UTF-8 (Rust `String::from_utf8`).
bool valid_utf8(std::string_view s) {
    std::size_t i = 0;
    const auto* p = reinterpret_cast<const unsigned char*>(s.data());
    while (i < s.size()) {
        const unsigned c = p[i];
        std::size_t len = 0;
        std::uint32_t cp = 0;
        if (c < 0x80) {
            ++i;
            continue;
        }
        if ((c & 0xE0) == 0xC0) len = 2, cp = c & 0x1F;
        else if ((c & 0xF0) == 0xE0) len = 3, cp = c & 0x0F;
        else if ((c & 0xF8) == 0xF0) len = 4, cp = c & 0x07;
        else return false;
        if (i + len > s.size()) return false;
        for (std::size_t k = 1; k < len; ++k) {
            if ((p[i + k] & 0xC0) != 0x80) return false;
            cp = (cp << 6) | (p[i + k] & 0x3F);
        }
        if ((len == 2 && cp < 0x80) || (len == 3 && cp < 0x800) || (len == 4 && (cp < 0x10000 || cp > 0x10FFFF)) ||
            (cp >= 0xD800 && cp <= 0xDFFF))
            return false;
        i += len;
    }
    return true;
}

std::string_view after_colon(const char* name) {
    std::string_view n(name);
    const auto colon = n.find(':');
    return colon == std::string_view::npos ? n : n.substr(colon + 1);
}

void walk(const pugi::xml_node& node, const std::function<XmlResult<void>(const pugi::xml_node&)>& f,
          XmlResult<void>& status) {
    for (auto child = node.first_child(); child && status; child = child.next_sibling()) {
        if (child.type() != pugi::node_element) continue;
        status = f(child);
        if (status) walk(child, f, status);
    }
}

} // namespace

struct Package::Impl {
    MemStream stream;
    unzFile zip = nullptr;
    ~Impl() {
        if (zip != nullptr) unzClose(zip);
    }
};

Package::Package(std::unique_ptr<Impl> impl, Format format) noexcept : impl_(std::move(impl)), format_(format) {}
Package::Package(Package&&) noexcept = default;
Package& Package::operator=(Package&&) noexcept = default;
Package::~Package() = default;

Result<Package> Package::open(std::span<const std::uint8_t> bytes, Format format) {
    auto impl = std::make_unique<Impl>();
    impl->stream.bytes = bytes;
    zlib_filefunc64_def funcs{};
    funcs.zopen64_file = mem_open;
    funcs.zread_file = mem_read;
    funcs.zwrite_file = mem_write;
    funcs.ztell64_file = mem_tell;
    funcs.zseek64_file = mem_seek;
    funcs.zclose_file = mem_close;
    funcs.zerror_file = mem_error;
    funcs.opaque = &impl->stream;
    impl->zip = unzOpen2_64("", &funcs);
    if (impl->zip == nullptr) return std::unexpected(DocsError::invalid(format, "invalid Zip archive: Could not find EOCD"));
    return Package(std::move(impl), format);
}

Result<std::optional<std::vector<std::uint8_t>>> Package::read(std::string_view name) {
    unzFile zip = impl_->zip;
    const std::string key(name);
    if (unzLocateFile(zip, key.c_str(), 1) != UNZ_OK) return std::optional<std::vector<std::uint8_t>>{};
    if (unzOpenCurrentFile(zip) != UNZ_OK) return std::unexpected(invalid(std::format("{}: unsupported Zip archive entry", name)));
    std::vector<std::uint8_t> out;
    std::uint8_t buf[64 * 1024];
    int n = 0;
    while ((n = unzReadCurrentFile(zip, buf, sizeof buf)) > 0) {
        out.insert(out.end(), buf, buf + n);
        if (out.size() > MAX_PART_BYTES) break;
    }
    const int closed = unzCloseCurrentFile(zip);
    if (out.size() > MAX_PART_BYTES)
        return std::unexpected(invalid(std::format("{} is larger than {} MiB", name, MAX_PART_BYTES >> 20)));
    if (n < 0) return std::unexpected(invalid(std::format("{}: invalid Zip archive: corrupt deflate stream", name)));
    if (closed == UNZ_CRCERROR) return std::unexpected(invalid(std::format("{}: Invalid checksum", name)));
    return std::optional<std::vector<std::uint8_t>>(std::move(out));
}

Result<std::optional<std::string>> Package::read_xml(std::string_view name) {
    auto bytes = read(name);
    if (!bytes) return std::unexpected(std::move(bytes.error()));
    if (!*bytes) return std::optional<std::string>{};
    std::string xml((*bytes)->begin(), (*bytes)->end());
    if (!valid_utf8(xml)) return std::unexpected(invalid(std::format("{} is not UTF-8 XML", name)));
    return std::optional<std::string>(std::move(xml));
}

Result<std::vector<Rel>> Package::rels(std::string_view part) {
    const auto slash = part.rfind('/');
    const std::string_view file = slash == std::string_view::npos ? part : part.substr(slash + 1);
    const std::string path = resolve_target(part, std::format("_rels/{}.rels", file));
    auto rels = parsed<std::vector<Rel>>(path, [&](std::string_view xml) -> XmlResult<std::vector<Rel>> {
        std::vector<Rel> out;
        auto walked = each_element(xml, [&](const pugi::xml_node& e) -> XmlResult<void> {
            if (local_name(e) != "Relationship" || attr(e, "TargetMode") == "External") return {};
            const auto id = attr(e, "Id");
            const auto kind = attr(e, "Type");
            const auto target = attr(e, "Target");
            if (!id || !kind || !target) return std::unexpected(std::string("Relationship lacks Id, Type or Target"));
            out.push_back(Rel{std::string(*id), std::string(*kind), resolve_target(part, *target)});
            return {};
        });
        if (!walked) return std::unexpected(std::move(walked.error()));
        return out;
    });
    if (!rels) return std::unexpected(std::move(rels.error()));
    return rels->value_or(std::vector<Rel>{});
}

std::string resolve_target(std::string_view source, std::string_view target) {
    if (target.starts_with('/')) return std::string(target.substr(1));
    std::vector<std::string_view> segments;
    const auto split = [](std::string_view s, auto&& f) {
        std::size_t start = 0;
        while (true) {
            const auto at = s.find('/', start);
            f(s.substr(start, at == std::string_view::npos ? std::string_view::npos : at - start));
            if (at == std::string_view::npos) break;
            start = at + 1;
        }
    };
    if (const auto slash = source.rfind('/'); slash != std::string_view::npos)
        split(source.substr(0, slash), [&](std::string_view s) { segments.push_back(s); });
    split(target, [&](std::string_view s) {
        if (s.empty() || s == ".") return;
        if (s == "..") {
            if (!segments.empty()) segments.pop_back();
            return;
        }
        segments.push_back(s);
    });
    std::string out;
    for (std::size_t i = 0; i < segments.size(); ++i) {
        if (i > 0) out.push_back('/');
        out.append(segments[i]);
    }
    return out;
}

XmlResult<std::unique_ptr<pugi::xml_document>> xml_reader(std::string_view xml) {
    if (xml.starts_with("\xEF\xBB\xBF")) xml.remove_prefix(3);
    auto doc = std::make_unique<pugi::xml_document>();
    const auto result = doc->load_buffer(xml.data(), xml.size(), pugi::parse_default | pugi::parse_ws_pcdata,
                                         pugi::encoding_utf8);
    if (!result) return std::unexpected(std::format("{} at position {}", result.description(), result.offset));
    return doc;
}

std::string_view local_name(const pugi::xml_node& node) { return after_colon(node.name()); }
std::string_view local_name(const pugi::xml_attribute& a) { return after_colon(a.name()); }

std::string_view prefix(const pugi::xml_attribute& a) {
    std::string_view n(a.name());
    const auto colon = n.find(':');
    return colon == std::string_view::npos ? std::string_view{} : n.substr(0, colon);
}

XmlResult<void> each_element(std::string_view xml, const std::function<XmlResult<void>(const pugi::xml_node&)>& f) {
    auto doc = xml_reader(xml);
    if (!doc) return std::unexpected(std::move(doc.error()));
    XmlResult<void> status;
    walk(**doc, f, status);
    return status;
}

std::optional<std::string_view> find_attr(const pugi::xml_node& e,
                                          const std::function<bool(const pugi::xml_attribute&)>& matches) {
    for (auto a = e.first_attribute(); a; a = a.next_attribute())
        if (matches(a)) return std::string_view(a.value());
    return std::nullopt;
}

std::optional<std::string_view> attr(const pugi::xml_node& e, std::string_view local) {
    return find_attr(e, [&](const pugi::xml_attribute& a) { return local_name(a) == local; });
}

XmlResult<std::optional<double>> num_attr(const pugi::xml_node& e, std::string_view local) {
    const auto v = attr(e, local);
    if (!v) return std::optional<double>{};
    std::string_view s = *v;
    const auto ws = [](char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'; };
    while (!s.empty() && ws(s.front())) s.remove_prefix(1);
    while (!s.empty() && ws(s.back())) s.remove_suffix(1);
    if (s.starts_with('+')) s.remove_prefix(1);
    std::int64_t n = 0;
    const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), n);
    if (s.empty() || ec != std::errc{} || end != s.data() + s.size() || (v->find("+-") != std::string_view::npos))
        return std::unexpected(std::format("attribute {}=\"{}\" is not a number", local, *v));
    return std::optional<double>(static_cast<double>(n));
}

} // namespace loa::surfaces::docs::ooxml
