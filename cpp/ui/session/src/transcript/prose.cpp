#include "loa/ui/session/transcript/prose.hpp"

#include <algorithm>
#include <cstdlib>

#include <md4c.h>

namespace loa::ui::session::transcript {

namespace md {

bool operator==(const Bullet& a, const Bullet& b) { return a.task == b.task && a.items == b.items; }
bool operator==(const Quote& a, const Quote& b) { return a.items == b.items; }

namespace {

void utf8(std::string& out, unsigned long cp) {
    if (cp == 0 || cp > 0x10FFFF || (cp >= 0xD800 && cp <= 0xDFFF)) cp = 0xFFFD;
    if (cp < 0x80) {
        out += static_cast<char>(cp);
    } else if (cp < 0x800) {
        out += static_cast<char>(0xC0 | (cp >> 6));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    } else if (cp < 0x10000) {
        out += static_cast<char>(0xE0 | (cp >> 12));
        out += static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    } else {
        out += static_cast<char>(0xF0 | (cp >> 18));
        out += static_cast<char>(0x80 | ((cp >> 12) & 0x3F));
        out += static_cast<char>(0x80 | ((cp >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (cp & 0x3F));
    }
}

// The common named entities and every numeric one; others stay as written.
std::string entity(std::string_view e) {
    std::string out;
    if (e.size() > 3 && e[1] == '#') {
        const bool hex = e[2] == 'x' || e[2] == 'X';
        const std::string digits(e.substr(hex ? 3 : 2, e.size() - (hex ? 4 : 3)));
        utf8(out, std::strtoul(digits.c_str(), nullptr, hex ? 16 : 10));
        return out;
    }
    static constexpr std::pair<std::string_view, std::string_view> NAMED[] = {
        {"&amp;", "&"}, {"&lt;", "<"}, {"&gt;", ">"}, {"&quot;", "\""}, {"&apos;", "'"}, {"&nbsp;", "\u00a0"},
        {"&copy;", "\u00a9"}, {"&mdash;", "\u2014"}, {"&ndash;", "\u2013"}, {"&hellip;", "\u2026"}};
    for (const auto& [name, value] : NAMED)
        if (name == e) return std::string(value);
    return std::string(e);
}

std::string attribute(const MD_ATTRIBUTE& a) { return std::string(a.text ? std::string_view(a.text, a.size) : std::string_view()); }

// One open container while md4c walks the document.
struct Frame {
    MD_BLOCKTYPE type = MD_BLOCK_DOC;
    std::vector<Block> blocks; // DOC, QUOTE, LI
    Text text;                 // P, H, TD/TH, and the loose text of a tight LI
    int level = 0;
    std::optional<std::string> language;
    std::string code;
    std::optional<unsigned> start;
    std::vector<Bullet> bullets;
    std::optional<bool> task;
    std::size_t header_rows = 0;
    bool in_head = false;
    std::vector<std::vector<Text>> rows;
};

struct Builder {
    std::vector<Frame> stack;
    int strong = 0, emphasis = 0, code = 0, strike = 0;
    std::vector<std::string> links;
    std::string_view source;
    // Columns of synthesized indentation not written yet, and the end of the last source code line.
    std::size_t indent = 0;
    const char* code_end = nullptr;

    Builder() { stack.emplace_back(); }

    Frame& top() { return stack.back(); }

    static bool holds_blocks(MD_BLOCKTYPE t) { return t == MD_BLOCK_DOC || t == MD_BLOCK_QUOTE || t == MD_BLOCK_LI; }

    // Turns the loose text of a tight list item into a paragraph.
    static void flush(Frame& f) {
        if (!f.text.empty() && holds_blocks(f.type)) {
            f.blocks.push_back(Block{Paragraph{std::move(f.text)}});
            f.text.clear();
        }
    }

    void push_block(Block b) {
        auto& f = top();
        flush(f);
        f.blocks.push_back(std::move(b));
    }

    void enter(MD_BLOCKTYPE type, void* detail) {
        if (type == MD_BLOCK_DOC) return;
        if (holds_blocks(top().type)) flush(top());
        Frame f;
        f.type = type;
        switch (type) {
        case MD_BLOCK_H: f.level = static_cast<int>(static_cast<MD_BLOCK_H_DETAIL*>(detail)->level); break;
        case MD_BLOCK_CODE: {
            const auto* d = static_cast<MD_BLOCK_CODE_DETAIL*>(detail);
            if (d->lang.size > 0) f.language = attribute(d->lang);
            break;
        }
        case MD_BLOCK_OL: f.start = static_cast<MD_BLOCK_OL_DETAIL*>(detail)->start; break;
        case MD_BLOCK_LI: {
            const auto* d = static_cast<MD_BLOCK_LI_DETAIL*>(detail);
            if (d->is_task) f.task = d->task_mark == 'x' || d->task_mark == 'X';
            break;
        }
        case MD_BLOCK_THEAD: top().in_head = true; return;
        case MD_BLOCK_TBODY: top().in_head = false; return;
        case MD_BLOCK_TR: top().rows.emplace_back(); if (top().in_head) ++top().header_rows; return;
        default: break;
        }
        stack.push_back(std::move(f));
    }

    void leave(MD_BLOCKTYPE type) {
        if (type == MD_BLOCK_THEAD || type == MD_BLOCK_TBODY || type == MD_BLOCK_TR || type == MD_BLOCK_DOC) return;
        Frame f = std::move(stack.back());
        stack.pop_back();
        flush(f);
        switch (type) {
        case MD_BLOCK_QUOTE: push_block(Block{Quote{std::move(f.blocks)}}); break;
        case MD_BLOCK_UL:
        case MD_BLOCK_OL: push_block(Block{List{f.start, std::move(f.bullets)}}); break;
        case MD_BLOCK_LI: top().bullets.push_back(Bullet{f.task, std::move(f.blocks)}); break;
        case MD_BLOCK_HR: push_block(Block{Rule{}}); break;
        case MD_BLOCK_H: push_block(Block{Heading{f.level, std::move(f.text)}}); break;
        case MD_BLOCK_CODE:
            if (indent > 0) f.code.append(indent, ' ');
            indent = 0;
            // An unclosed fence at the end of a source without a final newline gets no newline.
            if (code_end == source.data() + source.size() && !source.ends_with('\n') && f.code.ends_with('\n')) f.code.pop_back();
            push_block(Block{CodeBlock{std::move(f.language), std::move(f.code)}});
            break;
        case MD_BLOCK_HTML:
        case MD_BLOCK_P: push_block(Block{Paragraph{std::move(f.text)}}); break;
        case MD_BLOCK_TABLE: push_block(Block{Table{f.header_rows, std::move(f.rows)}}); break;
        case MD_BLOCK_TH:
        case MD_BLOCK_TD: top().rows.back().push_back(std::move(f.text)); break;
        default: break;
        }
    }

    [[nodiscard]] bool in_source(std::string_view s) const {
        return s.data() >= source.data() && s.data() + s.size() <= source.data() + source.size();
    }

    // md4c writes a code line's indentation as spaces, tabs expanded; puts back the author's
    // whitespace, less the columns the fence or container took.
    void flush_indent() {
        if (indent > 0) top().code.append(indent, ' ');
        indent = 0;
    }

    void code_text(std::string_view s) {
        auto& code = top().code;
        if (!in_source(s)) {
            if (!s.empty() && s.find_first_not_of(' ') == std::string_view::npos) {
                flush_indent();
                indent = s.size();
                return;
            }
            flush_indent();
            code += s;
            return;
        }
        if (indent > 0) {
            const auto at = static_cast<std::size_t>(s.data() - source.data());
            auto ws = at;
            while (ws > 0 && (source[ws - 1] == ' ' || source[ws - 1] == '\t')) --ws;
            const auto line = source.rfind('\n', ws == 0 ? std::string_view::npos : ws - 1);
            const auto line_start = line == std::string_view::npos || ws == 0 ? 0 : line + 1;
            const auto col = [&](std::size_t end) {
                std::size_t c = 0;
                for (auto i = line_start; i < end; ++i) c = source[i] == '\t' ? (c / 4 + 1) * 4 : c + 1;
                return c;
            };
            const auto end_col = col(at);
            if (source.substr(ws, at - ws).find('\t') != std::string_view::npos && end_col >= indent) {
                const auto keep_from = end_col - indent;
                std::size_t c = col(ws);
                for (auto i = ws; i < at; ++i) {
                    const auto next = source[i] == '\t' ? (c / 4 + 1) * 4 : c + 1;
                    if (c >= keep_from) {
                        code += source.substr(i, at - i);
                        break;
                    }
                    if (next > keep_from) {
                        code.append(next - keep_from, ' ');
                        code += source.substr(i + 1, at - i - 1);
                        break;
                    }
                    c = next;
                }
                indent = 0;
            }
            flush_indent();
        }
        code += s;
        code_end = s.data() + s.size();
    }

    void text(MD_TEXTTYPE type, std::string_view s) {
        auto& f = top();
        if (f.type == MD_BLOCK_CODE) return code_text(s);
        std::string value;
        switch (type) {
        case MD_TEXT_BR: value = "\n"; break;
        case MD_TEXT_SOFTBR: value = " "; break;
        case MD_TEXT_NULLCHAR: value = "\uFFFD"; break;
        case MD_TEXT_ENTITY: value = entity(s); break;
        default: value = std::string(s);
        }
        Span span{std::move(value), strong > 0, emphasis > 0, code > 0, strike > 0,
                  links.empty() ? std::nullopt : std::optional(links.back())};
        auto& text = f.text;
        if (!text.empty()) {
            auto& last = text.back();
            if (last.strong == span.strong && last.emphasis == span.emphasis && last.code == span.code &&
                last.strike == span.strike && last.link == span.link) {
                last.text += span.text;
                return;
            }
        }
        text.push_back(std::move(span));
    }

    void span(MD_SPANTYPE type, void* detail, bool enter) {
        const int d = enter ? 1 : -1;
        switch (type) {
        case MD_SPAN_EM: emphasis += d; break;
        case MD_SPAN_STRONG: strong += d; break;
        case MD_SPAN_CODE: code += d; break;
        case MD_SPAN_DEL: strike += d; break;
        case MD_SPAN_A:
            if (enter) links.push_back(attribute(static_cast<MD_SPAN_A_DETAIL*>(detail)->href));
            else links.pop_back();
            break;
        case MD_SPAN_IMG:
            if (enter) links.push_back(attribute(static_cast<MD_SPAN_IMG_DETAIL*>(detail)->src));
            else links.pop_back();
            break;
        default: break;
        }
    }
};

} // namespace

std::vector<Block> parse(std::string_view source) {
    Builder b;
    b.source = source;
    MD_PARSER parser{};
    parser.flags = MD_DIALECT_GITHUB;
    parser.enter_block = [](MD_BLOCKTYPE t, void* d, void* u) { static_cast<Builder*>(u)->enter(t, d); return 0; };
    parser.leave_block = [](MD_BLOCKTYPE t, void*, void* u) { static_cast<Builder*>(u)->leave(t); return 0; };
    parser.enter_span = [](MD_SPANTYPE t, void* d, void* u) { static_cast<Builder*>(u)->span(t, d, true); return 0; };
    parser.leave_span = [](MD_SPANTYPE t, void* d, void* u) { static_cast<Builder*>(u)->span(t, d, false); return 0; };
    parser.text = [](MD_TEXTTYPE t, const MD_CHAR* s, MD_SIZE n, void* u) {
        static_cast<Builder*>(u)->text(t, std::string_view(s, n));
        return 0;
    };
    (void)md_parse(source.data(), static_cast<MD_SIZE>(source.size()), &parser, &b);
    auto& doc = b.stack.front();
    Builder::flush(doc);
    return std::move(doc.blocks);
}

std::string plain(const Text& text) {
    std::string out;
    for (const auto& s : text) out += s.text;
    return out;
}

namespace {

void dump(std::string& out, const std::vector<Block>& blocks, int depth) {
    const std::string pad(static_cast<std::size_t>(depth) * 2, ' ');
    for (const auto& b : blocks) {
        std::visit(
            [&](const auto& x) {
                using T = std::decay_t<decltype(x)>;
                if constexpr (std::is_same_v<T, Paragraph>) out += pad + "P " + plain(x.text) + "\n";
                else if constexpr (std::is_same_v<T, Heading>) out += pad + "H" + std::to_string(x.level) + " " + plain(x.text) + "\n";
                else if constexpr (std::is_same_v<T, CodeBlock>) out += pad + "Code[" + x.language.value_or("") + "] " + x.code + "\n";
                else if constexpr (std::is_same_v<T, List>) {
                    out += pad + "List\n";
                    for (const auto& bullet : x.bullets) dump(out, bullet.items, depth + 1);
                } else if constexpr (std::is_same_v<T, Quote>) {
                    out += pad + "Quote\n";
                    dump(out, x.items, depth + 1);
                } else if constexpr (std::is_same_v<T, Rule>) out += pad + "Rule\n";
                else out += pad + "Table " + std::to_string(x.rows.size()) + "\n";
            },
            b.v);
    }
}

} // namespace

std::string debug(const std::vector<Block>& blocks) {
    std::string out;
    dump(out, blocks, 0);
    return out;
}

} // namespace md

std::string hard_breaks(std::string_view text) {
    std::string out;
    out.reserve(text.size() + 2 * static_cast<std::size_t>(std::count(text.begin(), text.end(), '\n')));
    while (!text.empty()) {
        const auto nl = text.find('\n');
        if (nl == std::string_view::npos) {
            out += text;
            break;
        }
        auto line = text.substr(0, nl);
        if (line.ends_with('\r')) line.remove_suffix(1);
        out += line;
        out += "  \n";
        text.remove_prefix(nl + 1);
    }
    return out;
}

std::string code_text(std::string_view code) {
    std::string out;
    out.reserve(code.size());
    while (!code.empty()) {
        const auto nl = code.find('\n');
        const auto line = code.substr(0, nl == std::string_view::npos ? code.size() : nl + 1);
        if (line.ends_with("  \n")) {
            out += line.substr(0, line.size() - 3);
            out += '\n';
        } else if (line == " \n") {
            // Indented fences can consume one of the two synthetic spaces on a blank line.
            out += '\n';
        } else {
            out += line;
        }
        code.remove_prefix(line.size());
    }
    return out;
}

namespace {

const std::string* find_code(const std::vector<md::Block>& blocks, std::size_t& index) {
    for (const auto& b : blocks) {
        if (const auto* c = std::get_if<md::CodeBlock>(&b.v)) {
            if (index == 0) return &c->code;
            --index;
        } else if (const auto* l = std::get_if<md::List>(&b.v)) {
            for (const auto& bullet : l->bullets)
                if (const auto* code = find_code(bullet.items, index)) return code;
        } else if (const auto* q = std::get_if<md::Quote>(&b.v)) {
            if (const auto* code = find_code(q->items, index)) return code;
        }
    }
    return nullptr;
}

} // namespace

std::optional<std::string> take_code(const std::vector<md::Block>& blocks, std::size_t index) {
    if (const auto* code = find_code(blocks, index)) return *code;
    return std::nullopt;
}

std::string copy_code(std::string_view source, std::size_t index, std::string_view shown) {
    if (auto code = take_code(md::parse(source), index)) return std::move(*code);
    return code_text(shown);
}

// ---- Streamed ------------------------------------------------------------------------------

Streamed::Streamed(std::string_view text) { push(text); }

void Streamed::append(std::string_view text, std::string_view chunk) {
    if (fed_ + chunk.size() == text.size() && text.ends_with(chunk)) push(chunk);
    else *this = Streamed(text);
}

void Streamed::push(std::string_view chunk) {
    source_ += hard_breaks(chunk);
    fed_ += chunk.size();
    lines_ += static_cast<std::size_t>(std::count(chunk.begin(), chunk.end(), '\n'));
    const auto old_cut = cut_;
    scan();
    blocks_.resize(stable_);
    if (cut_ != old_cut) {
        auto settled = md::parse(std::string_view(source_).substr(old_cut, cut_ - old_cut));
        blocks_.insert(blocks_.end(), std::make_move_iterator(settled.begin()), std::make_move_iterator(settled.end()));
        stable_ = blocks_.size();
    }
    auto tail = md::parse(std::string_view(source_).substr(cut_));
    blocks_.insert(blocks_.end(), std::make_move_iterator(tail.begin()), std::make_move_iterator(tail.end()));
}

namespace {

bool blank(std::string_view line) {
    return std::all_of(line.begin(), line.end(), [](char c) { return c == ' ' || c == '\t' || c == '\r' || c == '\n'; });
}

// A line that starts a block no earlier block can absorb: not indented, no list or quote
// marker, no table or setext continuation.
bool fresh(char c) {
    return c != ' ' && c != '\t' && c != '-' && c != '*' && c != '+' && c != '>' && c != '|' && c != '=' && c != '[' &&
           !(c >= '0' && c <= '9');
}

} // namespace

void Streamed::scan() {
    const std::string_view s(source_);
    auto consider = [&](std::size_t at) {
        if (at > cut_ && at < s.size() && prev_blank_ && fence_char_ == 0 && fresh(s[at])) cut_ = at;
    };
    while (true) {
        const auto nl = s.find('\n', scanned_);
        if (nl == std::string_view::npos) break;
        consider(scanned_);
        const auto line = s.substr(scanned_, nl - scanned_);
        // Fence open and close: up to three spaces, then three or more ` or ~.
        const auto indent = line.find_first_not_of(' ');
        if (indent != std::string_view::npos && indent <= 3 && (line[indent] == '`' || line[indent] == '~')) {
            const char c = line[indent];
            const auto run = line.find_first_not_of(c, indent);
            const auto len = (run == std::string_view::npos ? line.size() : run) - indent;
            if (len >= 3) {
                const auto rest = run == std::string_view::npos ? std::string_view() : line.substr(run);
                if (fence_char_ == 0) {
                    if (c != '`' || rest.find('`') == std::string_view::npos) fence_char_ = c, fence_len_ = len;
                } else if (c == fence_char_ && len >= fence_len_ && blank(rest)) {
                    fence_char_ = 0;
                }
            }
        }
        prev_blank_ = blank(line);
        scanned_ = nl + 1;
    }
    consider(scanned_);
}

} // namespace loa::ui::session::transcript
