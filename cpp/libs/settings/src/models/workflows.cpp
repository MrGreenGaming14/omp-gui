#include "loa/settings/models/workflows.hpp"

#include <algorithm>
#include <cstdio>

#include <asio/any_io_executor.hpp>
#include <asio/awaitable.hpp>

#include "loa/settings/models/config.hpp"
#include "text_util.hpp"

namespace loa::settings::models::workflows {

namespace {

namespace stdfs = std::filesystem;
using config::OmpConfig;
using detail::trim;
using detail::trim_end;
using detail::trim_start;
using selector::Selector;

constexpr std::string_view START = "<!-- omp-gui:workflow ";
constexpr std::string_view START_PREFIX = "<!-- omp-gui:workflow";
constexpr std::string_view END = "<!-- /omp-gui:workflow -->";
constexpr std::string_view END_PREFIX = "<!-- /omp-gui:workflow";
constexpr std::string_view WHEN = "When the active model role is `";
constexpr std::string_view ALLOWED = "- Allowed agents: ";
constexpr std::string_view DISALLOWED = "- Disallowed agents: ";
constexpr std::string_view FIELD = "<!-- loa:workflow-field -->";
constexpr std::string_view NOTES_FIELD = "<!-- loa:workflow-notes -->";

struct Span {
    std::size_t start = 0;
    std::size_t end = 0;
    friend bool operator==(Span, Span) = default;
};

struct Block {
    std::string role;
    Span span;
    Rules rules;
};

struct Parsed {
    std::vector<Block> blocks;
    bool crlf = false;
};

struct Line {
    std::size_t offset;
    std::string_view text;
};

Error malformed(std::string reason) { return Error::invalid("RULES.md", std::move(reason)); }

std::string_view slice(std::string_view s, Span span) { return s.substr(span.start, span.end - span.start); }

std::string replace_all(std::string_view s, std::string_view from, std::string_view to) {
    std::string out;
    out.reserve(s.size());
    std::size_t pos = 0;
    while (true) {
        const auto hit = s.find(from, pos);
        if (hit == std::string_view::npos) {
            out.append(s.substr(pos));
            return out;
        }
        out.append(s.substr(pos, hit - pos));
        out.append(to);
        pos = hit + from.size();
    }
}

std::string crlf_to_lf(std::string_view s) { return replace_all(s, "\r\n", "\n"); }

bool eq_ignore_ascii_case(std::string_view a, std::string_view b) {
    return std::ranges::equal(a, b, [](unsigned char x, unsigned char y) {
        const auto low = [](unsigned char c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; };
        return low(x) == low(y);
    });
}

bool contains_any(std::string_view s, std::string_view chars) { return s.find_first_of(chars) != std::string_view::npos; }

bool contains(const std::vector<std::string>& v, std::string_view item) { return std::ranges::find(v, item) != v.end(); }

std::string_view trim_matches_nl(std::string_view s) {
    while (!s.empty() && s.front() == '\n') {
        s.remove_prefix(1);
    }
    while (!s.empty() && s.back() == '\n') {
        s.remove_suffix(1);
    }
    return s;
}

std::optional<std::string_view> strip_prefix(std::string_view s, std::string_view p) {
    if (!s.starts_with(p)) {
        return std::nullopt;
    }
    return s.substr(p.size());
}

std::optional<std::string_view> strip_suffix(std::string_view s, std::string_view p) {
    if (!s.ends_with(p)) {
        return std::nullopt;
    }
    return s.substr(0, s.size() - p.size());
}

std::optional<std::pair<std::string_view, std::string_view>> split_once(std::string_view s, std::string_view sep) {
    const auto at = s.find(sep);
    if (at == std::string_view::npos) {
        return std::nullopt;
    }
    return std::pair{s.substr(0, at), s.substr(at + sep.size())};
}

// Rust `str::lines`.
std::vector<std::string_view> lines_of(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t pos = 0;
    while (pos < s.size()) {
        const auto nl = s.find('\n', pos);
        std::string_view line = nl == std::string_view::npos ? s.substr(pos) : s.substr(pos, nl - pos);
        if (line.ends_with('\r')) {
            line.remove_suffix(1);
        }
        out.push_back(line);
        if (nl == std::string_view::npos) {
            break;
        }
        pos = nl + 1;
    }
    return out;
}

std::string join(std::span<const Line> lines) {
    std::string out;
    for (std::size_t i = 0; i < lines.size(); ++i) {
        if (i > 0) {
            out += '\n';
        }
        out += lines[i].text;
    }
    return out;
}

std::optional<std::pair<std::size_t, std::string_view>> heading(std::string_view line) {
    std::size_t level = 0;
    while (level < line.size() && line[level] == '#') {
        ++level;
    }
    if (level >= 1 && level <= 6 && level < line.size() && line[level] == ' ') {
        return std::pair{level, trim(line.substr(level + 1))};
    }
    return std::nullopt;
}

std::vector<std::string> backticked(std::string_view line) {
    if (auto parts = split_once(line, ". ")) {
        line = parts->first;
    }
    std::vector<std::string> out;
    std::size_t index = 0;
    std::size_t pos = 0;
    while (true) {
        const auto tick = line.find('`', pos);
        const auto piece = line.substr(pos, tick == std::string_view::npos ? std::string_view::npos : tick - pos);
        if (index % 2 == 1) {
            out.emplace_back(piece);
        }
        ++index;
        if (tick == std::string_view::npos) {
            break;
        }
        pos = tick + 1;
    }
    return out;
}

std::optional<std::pair<std::string, ManagedFields>> structured_tail(std::span<const Line> lines) {
    const auto boundary = [](std::string_view line) { return line == FIELD || line == NOTES_FIELD; };
    std::size_t first = lines.size();
    for (std::size_t k = 0; k < lines.size(); ++k) {
        if (boundary(lines[k].text)) {
            first = k;
            break;
        }
    }
    std::string notes(trim_matches_nl(join(lines.subspan(0, first))));
    ManagedFields fields;
    std::size_t i = first;
    while (i < lines.size()) {
        if (lines[i].text == NOTES_FIELD) {
            if (!notes.empty() || i + 1 >= lines.size() || lines[i + 1].text != "### Notes" || i + 2 >= lines.size() ||
                !lines[i + 2].text.empty()) {
                return std::nullopt;
            }
            notes = join(lines.subspan(i + 3));
            break;
        }
        std::size_t end = lines.size();
        for (std::size_t j = i + 1; j < lines.size(); ++j) {
            if (boundary(lines[j].text)) {
                end = j;
                break;
            }
        }
        const auto section = lines.subspan(i + 1, end - (i + 1));
        if (section.empty()) {
            return std::nullopt;
        }
        const auto head = strip_prefix(section[0].text, "### ");
        if (!head) {
            return std::nullopt;
        }
        if (section.size() < 2 || !section[1].text.empty()) {
            return std::nullopt;
        }
        auto content_lines = section.subspan(2);
        if (end < lines.size() && !content_lines.empty() && content_lines.back().text.empty()) {
            content_lines = content_lines.first(content_lines.size() - 1);
        }
        const std::string content = join(content_lines);
        if (*head == "When to use" && fields.description.empty()) {
            fields.description = content;
        } else if (*head == "Routing" && fields.routing.empty()) {
            for (const auto line : lines_of(content)) {
                const auto rest = strip_prefix(line, "- `");
                if (!rest) {
                    return std::nullopt;
                }
                const auto parts = split_once(*rest, "`: ");
                if (!parts) {
                    return std::nullopt;
                }
                fields.routing.push_back({std::string(parts->first), std::string(parts->second)});
            }
        } else if (*head == "Steps" && fields.steps.empty()) {
            std::size_t index = 0;
            for (const auto line : lines_of(content)) {
                const auto step = strip_prefix(line, std::to_string(++index) + ". ");
                if (!step) {
                    return std::nullopt;
                }
                fields.steps.emplace_back(*step);
            }
        } else if (*head == "When to use" || *head == "Routing" || *head == "Steps") {
            return std::nullopt;
        } else {
            fields.blocks.push_back({std::string(*head), content});
        }
        i = end;
    }
    return std::pair{std::move(notes), std::move(fields)};
}

Result<std::string> render(std::string_view role, const Rules& rules, bool crlf);

Result<Parsed> parse(std::string_view text) {
    const auto first_nl = text.find('\n');
    const bool crlf = first_nl != std::string_view::npos && text.substr(0, first_nl).ends_with('\r');
    std::vector<Line> lines;
    std::size_t offset = 0;
    while (offset < text.size()) {
        const auto nl = text.find('\n', offset);
        const std::size_t end = nl == std::string_view::npos ? text.size() : nl + 1;
        std::string_view raw = text.substr(offset, end - offset);
        while (!raw.empty() && (raw.back() == '\r' || raw.back() == '\n')) {
            raw.remove_suffix(1);
        }
        lines.push_back({offset, raw});
        offset = end;
    }
    const auto line_no = [](std::size_t i) { return std::to_string(i + 1); };
    std::vector<Block> blocks;
    std::size_t i = 0;
    while (i < lines.size()) {
        const auto [start, line] = lines[i];
        const auto trimmed = trim(line);
        if (trimmed.starts_with(END_PREFIX)) {
            return std::unexpected(malformed("line " + line_no(i) + " has an unmatched workflow end marker"));
        }
        if (trimmed.starts_with(START_PREFIX)) {
            std::optional<std::string_view> role;
            if (auto rest = strip_prefix(trimmed, START)) {
                if (auto inner = strip_suffix(*rest, "-->")) {
                    const auto r = trim(*inner);
                    if (!r.empty() && !detail::any_char(r, detail::is_whitespace)) {
                        role = r;
                    }
                }
            }
            if (!role) {
                return std::unexpected(malformed("line " + line_no(i) + " has a malformed workflow start marker"));
            }
            std::optional<std::size_t> close;
            for (std::size_t j = i + 1; j < lines.size(); ++j) {
                const auto t = trim(lines[j].text);
                if (t.starts_with(START_PREFIX)) {
                    return std::unexpected(malformed("nested workflow start marker"));
                }
                if (t.starts_with(END_PREFIX)) {
                    if (t != END) {
                        return std::unexpected(malformed("malformed workflow end marker"));
                    }
                    close = j;
                    break;
                }
            }
            if (!close) {
                return std::unexpected(malformed("workflow start marker has no end marker"));
            }
            const std::span<const Line> body(lines.data() + i + 1, *close - (i + 1));
            // The first role declaration binds the marked section. Notes may quote other modes.
            std::optional<std::size_t> when;
            for (std::size_t k = 0; k < body.size(); ++k) {
                if (body[k].text.starts_with(WHEN)) {
                    when = k;
                    break;
                }
            }
            if (when) {
                if (auto parts = split_once(body[*when].text.substr(WHEN.size()), "`"); parts && parts->first != *role) {
                    return std::unexpected(malformed("workflow marker `" + std::string(*role) + "` does not match its role line"));
                }
            }
            const Span span{start, lines[*close].offset + lines[*close].text.size()};
            const std::size_t body_start = lines[i + 1].offset;
            std::optional<Rules> candidate;
            if (when) {
                std::string_view name = *role;
                for (const auto& l : body) {
                    if (auto h = strip_prefix(l.text, "## ")) {
                        if (auto n = strip_suffix(trim(*h), " Mode")) {
                            name = *n;
                        }
                        break;
                    }
                }
                name = trim(name);
                std::vector<std::string> allowed;
                std::vector<std::string> disallowed;
                std::size_t n = *when + 1;
                if (n < body.size() && body[n].text.empty()) {
                    ++n;
                }
                while (n < body.size()) {
                    const auto l = body[n].text;
                    if (l.starts_with(ALLOWED)) {
                        allowed = backticked(l);
                    } else if (l.starts_with(DISALLOWED)) {
                        disallowed = backticked(l);
                    } else {
                        break;
                    }
                    ++n;
                }
                if (auto tail = structured_tail(body.subspan(n))) {
                    candidate = Rules::managed(std::string(name), std::move(allowed), std::move(disallowed), std::move(tail->first),
                                               std::move(tail->second));
                }
            }
            if (candidate) {
                auto rendered = render(*role, *candidate, false);
                if (!rendered || *rendered != crlf_to_lf(slice(text, span))) {
                    candidate.reset();
                }
            }
            Rules rules = candidate ? std::move(*candidate)
                                    : Rules::external(std::string(text.substr(body_start, lines[*close].offset - body_start)));
            blocks.push_back({std::string(*role), span, std::move(rules)});
            i = *close + 1;
            continue;
        }
        if (auto h = heading(line); h && h->second.ends_with(" Mode")) {
            const std::size_t level = h->first;
            std::size_t end = i + 1;
            while (end < lines.size()) {
                const auto t = trim(lines[end].text);
                if (t.starts_with(START_PREFIX) || t.starts_with(END_PREFIX)) {
                    break;
                }
                if (auto other = heading(lines[end].text); other && other->first <= level) {
                    break;
                }
                ++end;
            }
            std::optional<std::string_view> role;
            for (std::size_t k = i + 1; k < end && !role; ++k) {
                if (auto parts = split_once(lines[k].text, WHEN)) {
                    if (auto r = split_once(parts->second, "`")) {
                        role = r->first;
                    }
                }
            }
            if (role) {
                const std::size_t finish = end < lines.size() ? lines[end].offset : text.size();
                blocks.push_back({std::string(*role), Span{start, finish},
                                  Rules::hand_written(std::string(text.substr(start, finish - start)))});
            }
            i = end;
            continue;
        }
        ++i;
    }
    for (std::size_t k = 0; k < blocks.size(); ++k) {
        for (std::size_t j = 0; j < k; ++j) {
            if (eq_ignore_ascii_case(blocks[j].role, blocks[k].role)) {
                return std::unexpected(malformed("the role `" + blocks[k].role + "` is defined more than once"));
            }
        }
    }
    return Parsed{std::move(blocks), crlf};
}

bool has_workflow_marker(std::string_view s) {
    return std::ranges::any_of(lines_of(s), [](std::string_view line) {
        line = trim_start(line);
        return line.starts_with(START_PREFIX) || line.starts_with(END_PREFIX);
    });
}

bool valid_text(std::string_view s) {
    return !has_workflow_marker(s) &&
           !std::ranges::any_of(lines_of(s), [](std::string_view line) { return line == FIELD || line == NOTES_FIELD; });
}

Result<std::string> render(std::string_view role, const Rules& rules, bool crlf) {
    switch (rules.kind) {
    case Rules::Kind::None:
        return std::string();
    case Rules::Kind::HandWritten: {
        if (rules.text.find("omp-gui:workflow") != std::string::npos) {
            return std::unexpected(Error::invalid("rules", "hand-written rules must not contain workflow markers"));
        }
        // The editor keeps old lines' endings and gives typed lines its own: the section takes the file's.
        auto lf = crlf_to_lf(rules.text);
        return crlf ? replace_all(lf, "\n", "\r\n") : lf;
    }
    case Rules::Kind::External:
        return std::unexpected(Error::invalid("rules", "external rules must retain their existing markers"));
    case Rules::Kind::Managed:
        break;
    }
    const auto& name = rules.name;
    const auto& allowed = rules.allowed;
    const auto& disallowed = rules.disallowed;
    const auto& fields = rules.fields;
    if (trim(name).empty() || trim(name) != name || contains_any(name, "\n\r`")) {
        return std::unexpected(Error::invalid("workflow name", "must be one line without backticks"));
    }
    const auto bad_agent = [](const std::string& a) { return trim(a).empty() || contains_any(a, "\n\r`"); };
    if (std::ranges::any_of(allowed, bad_agent) || std::ranges::any_of(disallowed, bad_agent)) {
        return std::unexpected(Error::invalid("agents", "invalid agent name"));
    }
    bool invalid = has_workflow_marker(rules.notes) || !valid_text(fields.description) ||
                   std::ranges::any_of(fields.blocks, [](const RuleBlock& b) { return !valid_text(b.text); }) ||
                   std::ranges::any_of(fields.steps, [](const std::string& s) { return s.empty() || contains_any(s, "\n\r") || !valid_text(s); });
    for (std::size_t index = 0; !invalid && index < fields.routing.size(); ++index) {
        const auto& r = fields.routing[index];
        invalid = !contains(allowed, r.agent) || r.note.empty() || contains_any(r.note, "\n\r") || !valid_text(r.note) ||
                  std::any_of(fields.routing.begin(), fields.routing.begin() + static_cast<std::ptrdiff_t>(index),
                              [&](const RoutingNote& other) { return other.agent == r.agent; });
    }
    invalid = invalid || std::ranges::any_of(fields.blocks, [](const RuleBlock& b) {
                  return b.heading.empty() || trim(b.heading) != b.heading || contains_any(b.heading, "\n\r") || b.heading == "When to use" ||
                         b.heading == "Routing" || b.heading == "Steps";
              });
    if (invalid) {
        return std::unexpected(Error::invalid("workflow fields", "invalid field text, routing agent, or block heading"));
    }
    std::string out;
    out.append(START).append(role).append(" -->\n## ").append(name).append(" Mode\n\n");
    out.append(WHEN).append(role).append("`, operate in ").append(name).append(" Mode.\n");
    const std::tuple<std::string_view, std::string_view, const std::vector<std::string>*> lists[] = {
        {ALLOWED, "Route work only to these agents.", &allowed},
        {DISALLOWED, "Never route work to these agents.", &disallowed},
    };
    for (const auto& [label, rule, agents] : lists) {
        if (agents->empty()) {
            continue;
        }
        if (out.ends_with("Mode.\n")) {
            out += '\n';
        }
        out += label;
        for (std::size_t k = 0; k < agents->size(); ++k) {
            if (k > 0) {
                out += ", ";
            }
            out.append("`").append((*agents)[k]).append("`");
        }
        out.append(". ").append(rule).append("\n");
    }
    const std::string notes_lf = crlf_to_lf(rules.notes);
    const std::string_view notes = trim_matches_nl(notes_lf);
    const bool marked_notes = std::ranges::any_of(lines_of(notes), [](std::string_view line) { return line == FIELD || line == NOTES_FIELD; }) ||
                              notes.starts_with(ALLOWED) || notes.starts_with(DISALLOWED);
    if (!notes.empty() && !marked_notes) {
        out.append("\n").append(notes).append("\n");
    }
    const auto section = [&](std::string_view head, std::string_view content) {
        out.append("\n").append(FIELD).append("\n### ").append(head).append("\n\n");
        out.append(content);
        out += '\n';
    };
    if (!fields.description.empty()) {
        section("When to use", crlf_to_lf(fields.description));
    }
    if (!fields.routing.empty()) {
        std::string routing;
        for (std::size_t k = 0; k < fields.routing.size(); ++k) {
            if (k > 0) {
                routing += '\n';
            }
            routing.append("- `").append(fields.routing[k].agent).append("`: ").append(fields.routing[k].note);
        }
        section("Routing", routing);
    }
    if (!fields.steps.empty()) {
        std::string steps;
        for (std::size_t k = 0; k < fields.steps.size(); ++k) {
            if (k > 0) {
                steps += '\n';
            }
            steps.append(std::to_string(k + 1)).append(". ").append(fields.steps[k]);
        }
        section("Steps", steps);
    }
    for (const auto& block : fields.blocks) {
        section(block.heading, crlf_to_lf(block.text));
    }
    // When copied Markdown contains reserved lines, a terminal Notes field takes the
    // remainder verbatim instead of interpreting those lines.
    if (marked_notes) {
        out.append("\n").append(NOTES_FIELD).append("\n### Notes\n\n").append(notes).append("\n");
    }
    out += END;
    return crlf ? replace_all(out, "\n", "\r\n") : out;
}

// Refuses `new_rules` unless every section other than the edited one reads as before
// (blank lines that now separate it from a new section aside) and the edited one spans
// `edited` (empty when it has no section).
Result<> check_blocks(std::string_view text, const Parsed& parsed, std::optional<std::string_view> previous_role, std::string_view new_rules,
                      const Workflow& workflow, Span edited) {
    auto after = parse(new_rules);
    if (!after) {
        return std::unexpected(std::move(after.error()));
    }
    std::vector<std::pair<std::string_view, std::string_view>> before;
    for (const auto& b : parsed.blocks) {
        if (!previous_role || b.role != *previous_role) {
            before.emplace_back(b.role, trim_end(slice(text, b.span)));
        }
    }
    std::vector<std::pair<std::string_view, std::string_view>> kept;
    const Block* block = nullptr;
    for (const auto& b : after->blocks) {
        if (b.role != workflow.role) {
            kept.emplace_back(b.role, trim_end(slice(new_rules, b.span)));
        } else if (block == nullptr) {
            block = &b;
        }
    }
    if (workflow.rules.kind == Rules::Kind::Managed && (block == nullptr || block->rules.kind != Rules::Kind::Managed)) {
        return std::unexpected(Error::invalid("workflow fields", "these fields cannot be read back as structured rules"));
    }
    const Span span = block != nullptr ? block->span : Span{edited.start, edited.start};
    if (before != kept || span != edited) {
        return std::unexpected(
            Error::invalid("rules", "the edit would change other sections of RULES.md: keep the section's heading level"));
    }
    return {};
}

Result<> check_one(const runtime::fs::Snapshot& snapshot) {
    auto current = runtime::fs::read_optional(snapshot.path());
    if (!current) {
        return std::unexpected(Error::io(snapshot.path(), current.error()));
    }
    if (*current != snapshot.contents()) {
        return std::unexpected(Error::changed_on_disk(snapshot.path()));
    }
    return {};
}

std::string_view contents_of(const runtime::fs::Snapshot& snapshot) {
    return snapshot.contents() ? std::string_view(*snapshot.contents()) : std::string_view();
}

} // namespace

Result<std::string> role_for(std::string_view name) {
    std::string role;
    bool gap = false;
    const auto trimmed = trim(name);
    for (std::size_t i = 0; i < trimmed.size();) {
        const auto c = detail::next_char(trimmed, i);
        const bool keep = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '-';
        if (keep) {
            if (gap && !role.empty()) {
                role += '_';
            }
            gap = false;
            role += static_cast<char>(c >= 'a' && c <= 'z' ? c - 32 : c);
        } else {
            gap = true;
        }
    }
    std::string_view r = role;
    while (!r.empty() && r.front() == '_') {
        r.remove_prefix(1);
    }
    while (!r.empty() && r.back() == '_') {
        r.remove_suffix(1);
    }
    std::string out(r);
    if (!selector::validate_role_name(out)) {
        return std::unexpected(Error::invalid("workflow name", "must contain letters or digits"));
    }
    return out;
}

namespace {

Result<> check_snapshot(const WorkflowFiles& files, const runtime::fs::Snapshot& config, const runtime::fs::Snapshot& rules) {
    if (config.path() != files.config || rules.path() != files.rules) {
        return std::unexpected(Error::invalid("snapshot", "snapshot belongs to different workflow files"));
    }
    if (auto ok = check_one(config); !ok) {
        return ok;
    }
    return check_one(rules);
}

} // namespace

Result<std::pair<std::vector<Workflow>, WorkflowSnapshot>> WorkflowFiles::load() const {
    auto rules_loaded = load_snapshot(rules);
    if (!rules_loaded) {
        return std::unexpected(std::move(rules_loaded.error()));
    }
    auto parsed = parse(rules_loaded->second.value_or(""));
    if (!parsed) {
        return std::unexpected(std::move(parsed.error()));
    }
    auto config_loaded = load_snapshot(config);
    if (!config_loaded) {
        return std::unexpected(std::move(config_loaded.error()));
    }
    auto cfg = OmpConfig::load(config);
    if (!cfg) {
        return std::unexpected(std::move(cfg.error()));
    }
    if (auto ok = check_one(config_loaded->first); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    auto roles = cfg->model_roles();
    if (!roles) {
        return std::unexpected(std::move(roles.error()));
    }
    std::vector<Workflow> workflows;
    for (auto& assignment : *roles) {
        Rules r;
        for (const auto& b : parsed->blocks) {
            if (b.role == assignment.role) {
                r = b.rules;
                break;
            }
        }
        workflows.push_back({std::move(assignment.role), std::move(assignment.candidates), std::move(r)});
    }
    return std::pair{std::move(workflows), WorkflowSnapshot(std::move(config_loaded->first), std::move(rules_loaded->first))};
}

namespace {

// Writes `config` (after validation) and then `new_rules`; restores `config.yml` when the
// RULES.md write fails and nobody changed `config.yml` in between.
contracts::Awaitable<Result<>> commit(const WorkflowFiles& files, OmpConfig config, const runtime::fs::Snapshot& config_snapshot,
                                      const runtime::fs::Snapshot& rules_snapshot, std::string new_rules, const SettingsValidator& validator) {
    if (auto ok = check_snapshot(files, config_snapshot, rules_snapshot); !ok) {
        co_return ok;
    }
    const bool edited = config.is_edited();
    if (edited) {
        if (auto ok = co_await config.validate(validator); !ok) {
            co_return ok;
        }
        if (auto ok = check_snapshot(files, config_snapshot, rules_snapshot); !ok) {
            co_return ok;
        }
        if (auto ok = config.save(); !ok) {
            co_return ok;
        }
    }
    // A models-only edit leaves RULES.md alone, absent or not.
    if (contents_of(rules_snapshot) == new_rules) {
        co_return Result<>{};
    }
    auto rules_copy = rules_snapshot;
    auto saved = save_snapshot(rules_copy, new_rules);
    if (saved) {
        co_return Result<>{};
    }
    const Error err = std::move(saved.error());
    if (!edited) {
        co_return std::unexpected(err);
    }
    auto current = read_optional(files.config);
    if (current && *current == std::optional<std::string>(config.text())) {
        std::optional<loa::Error> restore_error;
        if (const auto& bytes = config_snapshot.contents()) {
            if (auto ok = runtime::fs::write_atomic(files.config, *bytes); !ok) {
                restore_error = ok.error();
            }
        } else {
            std::error_code ec;
            if (!stdfs::remove(files.config, ec)) {
                restore_error = loa::Error(ErrorCode::io, ec ? ec.message() : "No such file or directory");
            }
        }
        if (!restore_error) {
            co_return std::unexpected(err);
        }
        co_return std::unexpected(Error::invalid("workflow", "writing RULES.md failed (" + err.to_string() + "); restoring config.yml failed (" +
                                                                 restore_error->to_string() + ")"));
    }
    co_return std::unexpected(Error::invalid(
        "workflow", "writing RULES.md failed (" + err.to_string() + "); config.yml changed afterwards and was not restored"));
}

struct Prepared {
    OmpConfig config;
    std::string new_rules;
};

Result<Prepared> prepare_save(const WorkflowFiles& files, const Workflow& workflow, std::optional<std::string_view> previous_role,
                              const runtime::fs::Snapshot& config_snapshot, const runtime::fs::Snapshot& rules_snapshot) {
    if (auto ok = check_snapshot(files, config_snapshot, rules_snapshot); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    if (auto ok = selector::validate_role_name(workflow.role); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    if (eq_ignore_ascii_case(workflow.role, "default")) {
        return std::unexpected(Error::invalid("role", "`default` is reserved"));
    }
    const std::string_view text = contents_of(rules_snapshot);
    auto parsed = parse(text);
    if (!parsed) {
        return std::unexpected(std::move(parsed.error()));
    }
    auto config = OmpConfig::load(files.config);
    if (!config) {
        return std::unexpected(std::move(config.error()));
    }
    if (auto ok = check_snapshot(files, config_snapshot, rules_snapshot); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    std::optional<config::RoleAssignment> old;
    if (previous_role) {
        auto found = config->model_role(*previous_role);
        if (!found) {
            return std::unexpected(std::move(found.error()));
        }
        old = std::move(*found);
        if (!old) {
            return std::unexpected(Error::invalid("role", "workflow role no longer exists"));
        }
    }
    const bool same_role = previous_role && *previous_role == workflow.role;
    const auto is_previous = [&](std::string_view role) { return previous_role && role == *previous_role; };
    if (!same_role) {
        auto roles = config->model_roles();
        if (!roles) {
            return std::unexpected(std::move(roles.error()));
        }
        const bool taken = std::ranges::any_of(*roles, [&](const auto& r) { return !is_previous(r.role) && eq_ignore_ascii_case(r.role, workflow.role); }) ||
                           std::ranges::any_of(parsed->blocks, [&](const Block& b) { return !is_previous(b.role) && eq_ignore_ascii_case(b.role, workflow.role); });
        if (taken) {
            return std::unexpected(Error::invalid("role", "role already exists (case insensitive)"));
        }
    }
    const Block* existing = nullptr;
    if (previous_role) {
        for (const auto& b : parsed->blocks) {
            if (b.role == *previous_role) {
                existing = &b;
                break;
            }
        }
    }
    if (existing != nullptr && existing->rules.kind != workflow.rules.kind) {
        return std::unexpected(Error::invalid("rules", "cannot convert between managed and hand-written sections"));
    }
    const std::string_view eol = parsed->crlf ? "\r\n" : "\n";
    std::string replacement;
    if (workflow.rules.kind == Rules::Kind::External) {
        if (existing == nullptr) {
            return std::unexpected(Error::invalid("rules", "external rules must retain their existing marker"));
        }
        if (!same_role) {
            return std::unexpected(Error::invalid("role", "an externally edited workflow cannot be renamed"));
        }
        const auto original = slice(text, existing->span);
        if (existing->rules == workflow.rules) {
            replacement = original;
        } else {
            const auto& body = workflow.rules.text;
            if (has_workflow_marker(body)) {
                return std::unexpected(Error::invalid("rules", "the raw text must not contain workflow markers"));
            }
            const auto first = original.find('\n');
            if (first == std::string_view::npos) {
                return std::unexpected(malformed("workflow marker has no body"));
            }
            const auto last = original.rfind('\n');
            const std::size_t start = first + 1;
            const std::size_t end = last + 1;
            const std::string_view boundary = body.empty() || body.ends_with('\n') ? "" : original.substr(0, start).ends_with("\r\n") ? "\r\n" : "\n";
            replacement.append(original.substr(0, start)).append(body).append(boundary).append(original.substr(end));
        }
    } else if (existing != nullptr && workflow.rules.kind == Rules::Kind::Managed && same_role && existing->rules == workflow.rules) {
        replacement = slice(text, existing->span);
    } else {
        auto rendered = render(workflow.role, workflow.rules, parsed->crlf);
        if (!rendered) {
            return std::unexpected(std::move(rendered.error()));
        }
        replacement = std::move(*rendered);
    }
    if (workflow.rules.kind == Rules::Kind::HandWritten) {
        if (previous_role && !same_role) {
            replacement = replace_all(replacement, "`" + std::string(*previous_role) + "`", "`" + workflow.role + "`");
        }
        // Text that follows must still start on its own line.
        if (!replacement.ends_with('\n') && (existing == nullptr || existing->span.end < text.size())) {
            replacement += eol;
        }
        auto checked = parse(replacement);
        if (!checked) {
            return std::unexpected(std::move(checked.error()));
        }
        if (checked->blocks.size() != 1 || checked->blocks[0].role != workflow.role || checked->blocks[0].span != Span{0, replacement.size()}) {
            return std::unexpected(Error::invalid("rules", "hand-written text must remain one mode section naming its role"));
        }
    }
    std::size_t start = 0;
    std::string new_rules;
    if (existing != nullptr) {
        start = existing->span.start;
        new_rules.append(text.substr(0, existing->span.start)).append(replacement).append(text.substr(existing->span.end));
    } else if (replacement.empty()) {
        start = text.size();
        new_rules = text;
    } else {
        new_rules = text;
        if (!new_rules.empty()) {
            if (!new_rules.ends_with('\n')) {
                new_rules += eol;
            }
            new_rules += eol;
        }
        start = new_rules.size();
        new_rules += replacement;
        if (!new_rules.ends_with('\n')) {
            new_rules += eol;
        }
    }
    if (auto ok = check_blocks(text, *parsed, previous_role, new_rules, workflow, Span{start, start + replacement.size()}); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    std::vector<Selector> candidates;
    if (!previous_role && workflow.models.empty()) {
        auto def = config->model_role("default");
        if (!def) {
            return std::unexpected(std::move(def.error()));
        }
        if (!*def) {
            return std::unexpected(Error::invalid("modelRoles", "there is no default role to copy"));
        }
        candidates = std::move((*def)->candidates);
    } else {
        candidates = workflow.models;
    }
    if (!same_role || (old && old->candidates != candidates)) {
        if (auto ok = config->set_model_role(workflow.role, candidates); !ok) {
            return std::unexpected(std::move(ok.error()));
        }
        if (previous_role && !same_role) {
            if (auto ok = config->clear_model_role(*previous_role); !ok) {
                return std::unexpected(std::move(ok.error()));
            }
        }
    }
    return Prepared{std::move(*config), std::move(new_rules)};
}

Result<Prepared> prepare_remove(const WorkflowFiles& files, std::string_view role, const runtime::fs::Snapshot& config_snapshot,
                                const runtime::fs::Snapshot& rules_snapshot) {
    if (auto ok = check_snapshot(files, config_snapshot, rules_snapshot); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    if (eq_ignore_ascii_case(role, "default")) {
        return std::unexpected(Error::invalid("role", "`default` is reserved"));
    }
    const std::string_view text = contents_of(rules_snapshot);
    auto parsed = parse(text);
    if (!parsed) {
        return std::unexpected(std::move(parsed.error()));
    }
    auto config = OmpConfig::load(files.config);
    if (!config) {
        return std::unexpected(std::move(config.error()));
    }
    if (auto ok = check_snapshot(files, config_snapshot, rules_snapshot); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    auto found = config->model_role(role);
    if (!found) {
        return std::unexpected(std::move(found.error()));
    }
    if (!*found) {
        return std::unexpected(Error::invalid("role", "workflow role does not exist"));
    }
    if (auto ok = config->clear_model_role(role); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    std::string new_rules(text);
    for (const auto& block : parsed->blocks) {
        if (block.role != role) {
            continue;
        }
        std::size_t start = block.span.start;
        std::size_t end = block.span.end;
        if (block.rules.kind == Rules::Kind::Managed) {
            const auto rest = text.substr(end);
            if (rest.starts_with("\r\n")) {
                end += 2;
            } else if (rest.starts_with('\n')) {
                end += 1;
            }
            const auto before = text.substr(0, start);
            const std::size_t len = before.ends_with("\r\n") ? 2 : before.ends_with('\n') ? 1 : 0;
            if (len > 0 && before.substr(0, before.size() - len).ends_with('\n')) {
                start -= len;
            }
        }
        new_rules = std::string(text.substr(0, start)) + std::string(text.substr(end));
        break;
    }
    return Prepared{std::move(*config), std::move(new_rules)};
}

} // namespace

contracts::Awaitable<Result<>> WorkflowFiles::save(const Workflow& workflow, std::optional<std::string_view> previous_role,
                                                   const WorkflowSnapshot& snapshot, const SettingsValidator& validator) const {
    auto prepared = prepare_save(*this, workflow, previous_role, snapshot.config_, snapshot.rules_);
    if (!prepared) {
        co_return std::unexpected(std::move(prepared.error()));
    }
    co_return co_await commit(*this, std::move(prepared->config), snapshot.config_, snapshot.rules_, std::move(prepared->new_rules), validator);
}

contracts::Awaitable<Result<>> WorkflowFiles::remove(std::string_view role, const WorkflowSnapshot& snapshot,
                                                     const SettingsValidator& validator) const {
    auto prepared = prepare_remove(*this, role, snapshot.config_, snapshot.rules_);
    if (!prepared) {
        co_return std::unexpected(std::move(prepared.error()));
    }
    co_return co_await commit(*this, std::move(prepared->config), snapshot.config_, snapshot.rules_, std::move(prepared->new_rules), validator);
}

} // namespace loa::settings::models::workflows
