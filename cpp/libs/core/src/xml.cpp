#include "loa/core/xml.hpp"

#include <charconv>
#include <cmath>
#include <format>
#include <iterator>
#include <variant>

#include "loa/core/base64.hpp"

namespace loa::core::xml {

namespace {

// Pending work of the walk; iterative so deep `<ext>` chains can't overflow.
struct Open {
    NodeId id;
    std::size_t depth;
};
struct Close {
    ElementKind kind;
    std::size_t depth;
};
using Step = std::variant<Open, Close>;

std::string_view suffix(const Value& value) {
    static constexpr std::string_view names[] = {"bool", "int", "float", "str", "bytes", "list"};
    return names[static_cast<std::size_t>(value.type())];
}

// Rust `{:?}` of an f64: shortest round trip, always a `.` or an exponent for
// finite values; exponent form below 1e-4 and from 1e16.
void format_float(std::string& buf, double v) {
    if (std::isnan(v)) {
        buf += "NaN";
        return;
    }
    if (std::isinf(v)) {
        buf += v < 0 ? "-inf" : "inf";
        return;
    }
    char tmp[64];
    const double a = std::fabs(v);
    if (a == 0.0 || (a >= 1e-4 && a < 1e16)) {
        auto [end, ec] = std::to_chars(tmp, tmp + sizeof tmp, v, std::chars_format::fixed);
        std::string_view s(tmp, end);
        buf += s;
        if (s.find('.') == std::string_view::npos) buf += ".0";
        return;
    }
    auto [end, ec] = std::to_chars(tmp, tmp + sizeof tmp, v, std::chars_format::scientific);
    std::string_view s(tmp, end);
    const auto e = s.find('e');
    buf += s.substr(0, e);
    buf += 'e';
    std::string_view exp = s.substr(e + 1);
    if (exp.front() == '-') {
        buf += '-';
        exp.remove_prefix(1);
    } else if (exp.front() == '+') {
        exp.remove_prefix(1);
    }
    while (exp.size() > 1 && exp.front() == '0') exp.remove_prefix(1);
    buf += exp;
}

// Formats a non-string attribute value, or a list item, into `buf`.
void format_value(std::string& buf, const Value& value) {
    std::visit(
        [&](const auto& v) {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, bool>) {
                buf += v ? "true" : "false";
            } else if constexpr (std::is_same_v<T, std::int64_t>) {
                std::format_to(std::back_inserter(buf), "{}", v);
            } else if constexpr (std::is_same_v<T, double>) {
                format_float(buf, v);
            } else if constexpr (std::is_same_v<T, Value::Bytes>) {
                base64::encode(v, buf);
            } else if constexpr (std::is_same_v<T, std::string>) {
                buf += '\'';
                for (char c : v) {
                    if (c == '\\' || c == '\'') buf += '\\';
                    buf += c;
                }
                buf += '\'';
            } else {
                buf += '[';
                for (std::size_t i = 0; i < v.size(); ++i) {
                    if (i > 0) buf += ", ";
                    if (auto bytes = v[i].as_bytes()) {
                        buf += "bytes(";
                        base64::encode(*bytes, buf);
                        buf += ')';
                    } else {
                        format_value(buf, v[i]);
                    }
                }
                buf += ']';
            }
        },
        value.variant());
}

// Decodes one UTF-8 scalar at `i`, advancing `i`. Input is valid UTF-8.
char32_t next_char(std::string_view s, std::size_t& i) {
    const auto b = static_cast<unsigned char>(s[i]);
    const int len = b < 0x80 ? 1 : b < 0xE0 ? 2 : b < 0xF0 ? 3 : 4;
    char32_t c = len == 1 ? b : b & (0x7F >> len);
    for (int k = 1; k < len && i + k < s.size(); ++k) c = (c << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3F);
    i += len;
    return c;
}

bool is_alpha(char32_t c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'); }

// Writes `name`, escaping what a plain XML name can't hold. Leaves out `:`
// (namespaces) and `.` (the type separator); `_x_` stands for the empty name.
void write_name(std::string& out, std::string_view name) {
    if (name.empty()) {
        out += "_x_";
        return;
    }
    for (std::size_t i = 0; i < name.size();) {
        const std::size_t at = i;
        const char32_t c = next_char(name, i);
        const bool plain = is_alpha(c) || c == '_' || (at > 0 && ((c >= '0' && c <= '9') || c == '-'));
        if (!plain || (c == '_' && at + 1 < name.size() && name[at + 1] == 'x'))
            std::format_to(std::back_inserter(out), "_x{:04X}_", static_cast<std::uint32_t>(c));
        else
            out += static_cast<char>(c);
    }
}

// Writes `text` with XML escapes, in runs so plain text is one append.
void escape(std::string& out, std::string_view text, bool in_attr) {
    std::size_t start = 0;
    for (std::size_t i = 0; i < text.size(); ++i) {
        const auto b = static_cast<unsigned char>(text[i]);
        std::string_view named;
        switch (b) {
        case '&': named = "&amp;"; break;
        case '<': named = "&lt;"; break;
        case '>': named = "&gt;"; break;
        case '"':
            if (!in_attr) continue;
            named = "&quot;";
            break;
        case '\n':
        case '\t':
            if (!in_attr) continue;
            break;
        default:
            if (b > 0x1F && b != 0x7F) continue;
        }
        out.append(text.substr(start, i - start));
        if (named.empty()) std::format_to(std::back_inserter(out), "&#x{:02X};", b);
        else out += named;
        start = i + 1;
    }
    out.append(text.substr(start));
}

} // namespace

std::string to_string(const Dom& dom) {
    std::string out;
    out.reserve(64 * dom.size() + 128);
    write(dom, out);
    return out;
}

void write(const Dom& dom, std::string& out) {
    out += "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
    std::format_to(std::back_inserter(out), "<!-- loa session dom: schema {}, seq {}, next nid {} -->\n",
                   SCHEMA_VERSION, dom.seq(), dom.next_id().value);
    std::string scratch;
    std::vector<Step> stack;
    if (const Node* root = dom.root()) stack.push_back(Open{root->id, 0});
    while (!stack.empty()) {
        const Step step = stack.back();
        stack.pop_back();
        if (auto* close = std::get_if<Close>(&step)) {
            std::format_to(std::back_inserter(out), "{:{}}</{}>\n", "", close->depth * 2, as_str(close->kind));
            continue;
        }
        const auto [id, depth] = std::get<Open>(step);
        const Node* node = dom.get(id);
        if (!node) continue;
        std::format_to(std::back_inserter(out), "{:{}}<{} nid=\"{}\"", "", depth * 2, as_str(node->kind),
                       node->id.value);
        for (const auto& [name, value] : node->attrs) {
            out += ' ';
            write_name(out, name);
            if (auto text = value.as_str()) {
                out += "=\"";
                escape(out, *text, true);
            } else {
                out += '.';
                out += suffix(value);
                out += "=\"";
                scratch.clear();
                format_value(scratch, value);
                escape(out, scratch, true);
            }
            out += '"';
        }
        if (node->children.empty()) {
            if (node->text.empty()) {
                out += "/>\n";
            } else {
                out += '>';
                escape(out, node->text, false);
                std::format_to(std::back_inserter(out), "</{}>\n", as_str(node->kind));
            }
        } else {
            out += '>';
            escape(out, node->text, false);
            out += '\n';
            stack.push_back(Close{node->kind, depth});
            for (auto it = node->children.rbegin(); it != node->children.rend(); ++it)
                stack.push_back(Open{*it, depth + 1});
        }
    }
}

} // namespace loa::core::xml
