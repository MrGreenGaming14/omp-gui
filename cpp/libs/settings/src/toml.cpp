// The one TU that includes toml++. Parsing converts toml++ nodes into plain
// values; writing follows `toml` 1.2 / `toml_writer` 1.2 output exactly.

#include "loa/settings/app/toml.hpp"

#include <charconv>
#include <cmath>
#include <cstdio>
#include <sstream>

#include <toml++/toml.hpp>

namespace loa::settings::app::toml {

std::string_view Value::type_name() const noexcept {
    switch (data.index()) {
    case 0: return "string";
    case 1: return "integer";
    case 2: return "floating point";
    case 3: return "boolean";
    case 4: return "datetime";
    case 5: return "sequence";
    default: return "map";
    }
}

namespace {

std::string two(unsigned v) {
    char buf[8];
    std::snprintf(buf, sizeof buf, "%02u", v);
    return buf;
}

std::string date_text(const ::toml::date& d) {
    char buf[16];
    std::snprintf(buf, sizeof buf, "%04u-%02u-%02u", unsigned(d.year), unsigned(d.month), unsigned(d.day));
    return buf;
}

std::string time_text(const ::toml::time& t) {
    std::string s = two(t.hour) + ":" + two(t.minute) + ":" + two(t.second);
    if (t.nanosecond != 0) {
        char buf[16];
        std::snprintf(buf, sizeof buf, "%09u", unsigned(t.nanosecond));
        std::string frac = buf;
        while (frac.size() > 1 && frac.back() == '0') frac.pop_back();
        s += "." + frac;
    }
    return s;
}

std::string offset_text(const ::toml::time_offset& o) {
    if (o.minutes == 0) return "Z";
    int minutes = o.minutes;
    char sign = '+';
    if (minutes < 0) {
        minutes = -minutes;
        sign = '-';
    }
    return std::string(1, sign) + two(unsigned(minutes / 60)) + ":" + two(unsigned(minutes % 60));
}

Value convert(const ::toml::node& node);

Table convert_table(const ::toml::table& t) {
    Table out;
    for (auto&& [k, v] : t) out.emplace(std::string(k.str()), convert(v));
    return out;
}

Value convert(const ::toml::node& node) {
    switch (node.type()) {
    case ::toml::node_type::table: return Value(convert_table(*node.as_table()));
    case ::toml::node_type::array: {
        Array out;
        for (auto&& v : *node.as_array()) out.push_back(convert(v));
        return Value(std::move(out));
    }
    case ::toml::node_type::string: return Value(std::string(node.as_string()->get()));
    case ::toml::node_type::integer: return Value(std::int64_t{node.as_integer()->get()});
    case ::toml::node_type::floating_point: return Value(node.as_floating_point()->get());
    case ::toml::node_type::boolean: return Value(node.as_boolean()->get());
    case ::toml::node_type::date: return Value(Datetime{date_text(node.as_date()->get())});
    case ::toml::node_type::time: return Value(Datetime{time_text(node.as_time()->get())});
    case ::toml::node_type::date_time: {
        const auto& dt = node.as_date_time()->get();
        std::string s = date_text(dt.date) + "T" + time_text(dt.time);
        if (dt.offset) s += offset_text(*dt.offset);
        return Value(Datetime{std::move(s)});
    }
    default: return Value();
    }
}

bool bare_byte(unsigned char b) {
    return (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || (b >= '0' && b <= '9') || b == '-' || b == '_';
}

void write_escaped(std::string& out, std::string_view s, bool ml) {
    const int max_quotes = ml ? 2 : 0;
    int quotes = 0;
    for (unsigned char b : s) {
        if (b == '"') {
            if (++quotes > max_quotes) {
                out += "\\\"";
                quotes = 0;
                continue;
            }
        } else {
            quotes = 0;
        }
        switch (b) {
        case 0x8: out += "\\b"; break;
        case 0x9: out += "\\t"; break;
        case 0xa: out += ml ? "\n" : "\\n"; break;
        case 0xc: out += "\\f"; break;
        case 0xd: out += "\\r"; break;
        case 0x5c: out += "\\\\"; break;
        default:
            if (b <= 0x1f || b == 0x7f) {
                char buf[8];
                std::snprintf(buf, sizeof buf, "\\u%04X", unsigned(b));
                out += buf;
            } else {
                out += char(b);
            }
        }
    }
}

void write_string(std::string& out, std::string_view s) {
    bool subset = true;
    for (unsigned char b : s)
        if (!bare_byte(b) && b != '.' && b != '/' && b != ' ' && b != ':') subset = false;
    if (subset) {
        out += '"';
        out += s;
        out += '"';
        return;
    }
    int max_single = 0, max_double = 0, run_single = 0, run_double = 0;
    bool escape_codes = false, escape = false, newline = false;
    for (unsigned char b : s) {
        run_single = b == '\'' ? run_single + 1 : 0;
        run_double = b == '"' ? run_double + 1 : 0;
        max_single = std::max(max_single, run_single);
        max_double = std::max(max_double, run_double);
        if (b == '\\') escape = true;
        else if (b == '\t') {
        } else if (b == '\n') newline = true;
        else if (b <= 0x1f || b == 0x7f) escape_codes = true;
    }
    if (!escape_codes && !escape && max_double == 0 && !newline) { // basic, pretty
        out += '"';
        write_escaped(out, s, false);
        out += '"';
    } else if (!escape_codes && max_single == 0 && !newline) { // literal
        out += '\'';
        out += s;
        out += '\'';
    } else if (!escape_codes && !escape && max_double <= 2) { // multi-line basic, pretty
        out += "\"\"\"";
        if (newline) out += '\n';
        write_escaped(out, s, true);
        out += "\"\"\"";
    } else if (!escape_codes && max_single <= 2) { // multi-line literal
        out += "'''";
        if (newline) out += '\n';
        out += s;
        out += "'''";
    } else if (newline) {
        out += "\"\"\"\n";
        write_escaped(out, s, true);
        out += "\"\"\"";
    } else {
        out += '"';
        write_escaped(out, s, false);
        out += '"';
    }
}

void write_key(std::string& out, std::string_view k) {
    bool bare = !k.empty();
    bool single = false, dbl = false, codes = false, esc = false;
    for (unsigned char b : k) {
        if (!bare_byte(b)) bare = false;
        if (b == '\'') single = true;
        else if (b == '"') dbl = true;
        else if (b == '\\') esc = true;
        else if (b != '\t' && (b <= 0x1f || b == 0x7f)) codes = true;
    }
    if (bare) {
        out += k;
    } else if (!codes && !esc && !dbl) {
        out += '"';
        out += k;
        out += '"';
    } else if (!codes && !single) {
        out += '\'';
        out += k;
        out += '\'';
    } else {
        out += '"';
        write_escaped(out, k, false);
        out += '"';
    }
}

void write_float(std::string& out, double v) {
    if (std::isnan(v)) {
        out += std::signbit(v) ? "-nan" : "nan";
    } else if (v == 0.0) {
        out += std::signbit(v) ? "-0.0" : "0.0";
    } else if (std::isinf(v)) {
        out += v < 0 ? "-inf" : "inf";
    } else {
        char buf[400];
        auto r = std::to_chars(buf, buf + sizeof buf, v, std::chars_format::fixed);
        out.append(buf, r.ptr);
        if (std::fmod(v, 1.0) == 0.0) out += ".0";
    }
}

void write_inline(std::string& out, const Value& v) {
    std::visit(
        [&](const auto& x) {
            using T = std::decay_t<decltype(x)>;
            if constexpr (std::is_same_v<T, std::string>) write_string(out, x);
            else if constexpr (std::is_same_v<T, std::int64_t>) out += std::to_string(x);
            else if constexpr (std::is_same_v<T, double>) write_float(out, x);
            else if constexpr (std::is_same_v<T, bool>) out += x ? "true" : "false";
            else if constexpr (std::is_same_v<T, Datetime>) out += x.text;
            else if constexpr (std::is_same_v<T, Array>) {
                out += '[';
                for (std::size_t i = 0; i < x.size(); ++i) {
                    if (i) out += ", ";
                    write_inline(out, x[i]);
                }
                out += ']';
            } else {
                out += '{';
                bool first = true;
                for (const auto& [k, e] : x) {
                    out += first ? " " : ", ";
                    first = false;
                    write_key(out, k);
                    out += " = ";
                    write_inline(out, e);
                }
                if (!first) out += ' ';
                out += '}';
            }
        },
        v.data);
}

bool is_table_array(const Value& v) {
    const Array* a = v.as_array();
    if (!a || a->empty()) return false;
    for (const auto& e : *a)
        if (!e.as_table()) return false;
    return true;
}

std::string header_path(const std::vector<std::string>& path) {
    std::string s;
    for (std::size_t i = 0; i < path.size(); ++i) {
        if (i) s += '.';
        write_key(s, path[i]);
    }
    return s;
}

void write_body(std::string& out, const Table& t) {
    for (const auto& [k, v] : t) {
        if (v.as_table() || is_table_array(v)) continue;
        write_key(out, k);
        out += " = ";
        write_inline(out, v);
        out += '\n';
    }
}

void write_tables(std::string& out, const Table& t, std::vector<std::string>& path);

void write_section(std::string& out, const Table& t, std::vector<std::string>& path, bool array_item) {
    bool has_values = false, has_children = false;
    for (const auto& [k, v] : t) {
        if (v.as_table() || is_table_array(v)) has_children = true;
        else has_values = true;
    }
    if (array_item || has_values || !has_children) {
        if (!out.empty()) out += '\n';
        out += array_item ? "[[" : "[";
        out += header_path(path);
        out += array_item ? "]]\n" : "]\n";
        write_body(out, t);
    }
    write_tables(out, t, path);
}

void write_tables(std::string& out, const Table& t, std::vector<std::string>& path) {
    for (const auto& [k, v] : t) {
        if (const Table* sub = v.as_table()) {
            path.push_back(k);
            write_section(out, *sub, path, false);
            path.pop_back();
        } else if (is_table_array(v)) {
            path.push_back(k);
            for (const auto& e : *v.as_array()) write_section(out, *e.as_table(), path, true);
            path.pop_back();
        }
    }
}

} // namespace

std::expected<Table, std::string> parse(std::string_view text) {
#if TOML_EXCEPTIONS
    try {
        return convert_table(::toml::parse(text));
    } catch (const ::toml::parse_error& err) {
#else
    ::toml::parse_result result = ::toml::parse(text);
    if (result) return convert_table(result.table());
    {
        const auto& err = result.error();
#endif
        std::ostringstream msg;
        msg << "TOML parse error at line " << err.source().begin.line << ", column " << err.source().begin.column
            << ": " << err.description();
        return std::unexpected(msg.str());
    }
}

std::string write(const Table& doc) {
    std::string out;
    write_body(out, doc);
    std::vector<std::string> path;
    write_tables(out, doc, path);
    return out;
}

std::string write_value(const Value& value) {
    std::string out;
    write_inline(out, value);
    return out;
}

} // namespace loa::settings::app::toml
