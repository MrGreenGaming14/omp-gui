#include "loa/settings/models/agents.hpp"

#include <algorithm>
#include <system_error>

#include "loa/runtime/fs.hpp"
#include "loa/settings/models/selector.hpp"
#include "text_util.hpp"

namespace loa::settings::models::agents {

namespace {

namespace stdfs = std::filesystem;
using yaml::Yaml;

constexpr std::size_t MAX_NAME_LEN = 64;

using detail::trim;
using detail::trim_end;

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

std::vector<std::string> strings_of(const yaml::Array& items) {
    std::vector<std::string> out;
    for (const auto& item : items) {
        if (const auto* s = item.as_str()) {
            out.push_back(*s);
        }
    }
    return out;
}

loa::Error io_error(const std::error_code& ec) {
    return loa::Error(ec == std::errc::no_such_file_or_directory ? ErrorCode::not_found : ErrorCode::io, ec.message());
}

bool exists_no_follow(const stdfs::path& path) {
    std::error_code ec;
    return stdfs::exists(stdfs::symlink_status(path, ec));
}

std::optional<std::string> safe_stem_problem(std::string_view name) {
    if (name.empty()) {
        return "is required";
    }
    if (name.size() > MAX_NAME_LEN) {
        return "must be at most " + std::to_string(MAX_NAME_LEN) + " characters";
    }
    if (name.front() == '.') {
        return "must not start with '.'";
    }
    const bool safe = std::ranges::all_of(name, [](char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.';
    });
    if (!safe) {
        return "may only contain ASCII letters, digits, '-', '_' and '.'";
    }
    return std::nullopt;
}

stdfs::path disabled_twin(const stdfs::path& path) {
    stdfs::path out = path;
    out += ".disabled";
    return out;
}

std::string lower(std::string_view s) {
    std::string out(s);
    std::ranges::transform(out, out.begin(), [](unsigned char c) { return static_cast<char>(c >= 'A' && c <= 'Z' ? c + 32 : c); });
    return out;
}

void scan(const stdfs::path& dir, AgentSource source, AgentList& out) {
    std::error_code ec;
    stdfs::directory_iterator it(dir, ec);
    if (ec) {
        if (ec != std::errc::no_such_file_or_directory) {
            out.broken.emplace_back(dir, Error::io(dir, io_error(ec)));
        }
        return;
    }
    for (const auto& entry : it) {
        const auto& path = entry.path();
        const std::string file_name = path.filename().string();
        bool enabled = false;
        if (file_name.ends_with(".md")) {
            enabled = true;
        } else if (file_name.ends_with(".md.disabled")) {
            enabled = false;
        } else {
            continue;
        }
        std::error_code type_ec;
        if (!stdfs::is_regular_file(path, type_ec)) {
            continue;
        }
        auto text = read_optional(path);
        if (!text) {
            out.broken.emplace_back(path, std::move(text.error()));
            continue;
        }
        if (!*text) {
            out.broken.emplace_back(path, Error::io(path, loa::Error(ErrorCode::not_found, "No such file or directory")));
            continue;
        }
        auto definition = parse(**text);
        if (!definition) {
            out.broken.emplace_back(path, std::move(definition.error()));
            continue;
        }
        out.files.push_back(AgentFile{std::move(*definition), path, source, enabled});
    }
}

} // namespace

Result<AgentDefinition> AgentDefinition::create(std::string_view name, std::string_view description) {
    auto front = yaml::YamlDocument::parse("", "agent");
    if (!front) {
        return std::unexpected(std::move(front.error()));
    }
    AgentDefinition def("---\n", "---\n", false, std::move(*front), {});
    if (auto ok = def.set_name(name); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    if (auto ok = def.set_description(description); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    return def;
}

std::optional<std::string_view> AgentDefinition::str_key(std::string_view key) const {
    const Yaml* v = front_.get({key});
    if (v == nullptr) {
        return std::nullopt;
    }
    if (const auto* s = v->as_str()) {
        return std::string_view(*s);
    }
    return std::nullopt;
}

std::string_view AgentDefinition::name() const { return str_key("name").value_or(""); }

std::string_view AgentDefinition::description() const { return str_key("description").value_or(""); }

std::optional<std::string_view> AgentDefinition::thinking_level() const { return str_key("thinkingLevel"); }

std::optional<ModelSpec> AgentDefinition::model() const {
    const Yaml* v = front_.get({"model"});
    if (v == nullptr) {
        return std::nullopt;
    }
    if (const auto* s = v->as_str()) {
        return ModelSpec::one(*s);
    }
    if (const auto* items = v->as_vec()) {
        return ModelSpec::chain(strings_of(*items));
    }
    return std::nullopt;
}

std::optional<std::vector<std::string>> AgentDefinition::list_key(std::string_view key) const {
    const Yaml* v = front_.get({key});
    if (v == nullptr) {
        return std::nullopt;
    }
    if (const auto* items = v->as_vec()) {
        return strings_of(*items);
    }
    if (const auto* s = v->as_str()) {
        return std::vector<std::string>{*s};
    }
    return std::nullopt;
}

Result<> AgentDefinition::set_name(std::string_view name) { return front_.set({"name"}, yaml::ystr(std::string(name))); }

Result<> AgentDefinition::set_description(std::string_view description) {
    return front_.set({"description"}, yaml::ystr(std::string(description)));
}

Result<> AgentDefinition::set_model(std::optional<ModelSpec> model) {
    if (!model) {
        auto removed = front_.remove({"model"});
        if (!removed) {
            return std::unexpected(std::move(removed.error()));
        }
        return {};
    }
    if (model->kind == ModelSpec::Kind::One) {
        return front_.set({"model"}, yaml::ystr(model->items.empty() ? std::string() : model->items.front()));
    }
    return front_.set({"model"}, yaml::ystrs(std::span<const std::string>(model->items)));
}

Result<> AgentDefinition::set_thinking_level(std::optional<std::string_view> level) {
    if (!level) {
        auto removed = front_.remove({"thinkingLevel"});
        if (!removed) {
            return std::unexpected(std::move(removed.error()));
        }
        return {};
    }
    return front_.set({"thinkingLevel"}, yaml::ystr(std::string(*level)));
}

Result<> AgentDefinition::set_list(std::string_view key, std::optional<std::vector<std::string>> list) {
    if (!list) {
        auto removed = front_.remove({key});
        if (!removed) {
            return std::unexpected(std::move(removed.error()));
        }
        return {};
    }
    return front_.set({key}, yaml::ystrs(std::span<const std::string>(*list)));
}

std::string AgentDefinition::to_text() const {
    const std::string& front = front_.text();
    std::string out;
    out.reserve(open_.size() + front.size() + close_.size() + body_.size() + 16);
    out += open_;
    if (crlf_) {
        out += replace_all(front, "\n", "\r\n");
    } else {
        out += front;
    }
    out += close_;
    out += body_;
    return out;
}

Result<AgentDefinition> parse(std::string_view text) {
    const auto bad = [](std::string message) { return std::unexpected(Error::parse("agent file", std::move(message))); };
    std::size_t pos = 0;
    // The next line including its `\n`, empty at the end.
    const auto next_line = [&]() -> std::optional<std::string_view> {
        if (pos >= text.size()) {
            return std::nullopt;
        }
        const auto nl = text.find('\n', pos);
        const std::size_t end = nl == std::string_view::npos ? text.size() : nl + 1;
        const auto line = text.substr(pos, end - pos);
        pos = end;
        return line;
    };
    const auto open = next_line();
    if (!open || trim_end(*open) != "---") {
        return bad("the first line must be `---`");
    }
    const bool crlf = open->ends_with("\r\n");
    std::string front;
    std::optional<std::string_view> close;
    while (const auto line = next_line()) {
        if (trim_end(*line) == "---") {
            close = line;
            break;
        }
        front += *line;
    }
    if (!close) {
        return bad("the frontmatter is not closed by a `---` line");
    }
    if (crlf) {
        front = replace_all(front, "\r\n", "\n");
    }
    auto doc = yaml::YamlDocument::parse(front, "agent frontmatter");
    if (!doc) {
        return std::unexpected(std::move(doc.error()));
    }
    return AgentDefinition(std::string(*open), std::string(*close), crlf, std::move(*doc), std::string(text.substr(pos)));
}

AgentList list(const std::filesystem::path& agent_dir, const std::optional<std::filesystem::path>& project) {
    AgentList out;
    scan(agent_dir / "agents", AgentSource::User, out);
    if (project) {
        scan(*project / ".omp" / "agents", AgentSource::Project, out);
    }
    std::ranges::stable_sort(out.files, [](const AgentFile& a, const AgentFile& b) {
        const auto la = lower(a.definition.name());
        const auto lb = lower(b.definition.name());
        if (la != lb) {
            return la < lb;
        }
        return a.path < b.path;
    });
    std::ranges::stable_sort(out.broken, [](const auto& a, const auto& b) { return a.first < b.first; });
    return out;
}

std::vector<FieldError> validate(const AgentDefinition& def, std::span<const std::string> known_tools,
                                 std::span<const std::string> known_agents) {
    std::vector<FieldError> errors;
    const auto contains = [](std::span<const std::string> list, std::string_view item) {
        return std::ranges::find(list, item) != list.end();
    };
    const std::string_view name = def.name();
    if (auto problem = safe_stem_problem(name)) {
        errors.push_back({"name", "name " + *problem});
    }
    if (trim(def.description()).empty()) {
        errors.push_back({"description", "description is required"});
    }
    if (auto tools = def.tools(); tools && !known_tools.empty()) {
        for (const auto& tool : *tools) {
            if (!contains(known_tools, tool)) {
                errors.push_back({"tools", "unknown tool '" + tool + "'"});
            }
        }
    }
    if (auto spawns = def.spawns()) {
        for (const auto& spawn : *spawns) {
            if (spawn == name) {
                errors.push_back({"spawns", "an agent cannot spawn itself"});
            } else if (!known_agents.empty() && !contains(known_agents, spawn)) {
                errors.push_back({"spawns", "unknown agent '" + spawn + "'"});
            }
        }
    }
    if (auto level = def.thinking_level(); level && std::ranges::find(selector::THINKING_LEVELS, *level) == selector::THINKING_LEVELS.end()) {
        errors.push_back({"thinkingLevel", "unknown thinking level '" + std::string(*level) + "'"});
    }
    if (auto model = def.model()) {
        const auto entries = model->entries();
        if (entries.empty() || std::ranges::any_of(entries, [](const std::string& m) { return trim(m).empty(); })) {
            errors.push_back({"model", "model entries must not be empty"});
        }
    }
    return errors;
}

Result<std::filesystem::path> write(const std::filesystem::path& dir, const AgentDefinition& def, Overwrite overwrite) {
    const std::string_view name = def.name();
    if (auto problem = safe_stem_problem(name)) {
        return std::unexpected(Error::invalid("name", *problem));
    }
    const auto path = dir / (std::string(name) + ".md");
    if (overwrite == Overwrite::No) {
        for (const auto& candidate : {path, disabled_twin(path)}) {
            if (exists_no_follow(candidate)) {
                return std::unexpected(Error::already_exists(candidate));
            }
        }
    }
    std::error_code ec;
    stdfs::create_directories(dir, ec);
    if (ec) {
        return std::unexpected(Error::io(path, io_error(ec)));
    }
    if (auto ok = runtime::fs::write_atomic(path, def.to_text()); !ok) {
        return std::unexpected(Error::io(path, ok.error()));
    }
    return path;
}

Result<std::filesystem::path> set_enabled(const std::filesystem::path& path, bool enabled) {
    const std::string name = path.string();
    stdfs::path target;
    if (enabled) {
        constexpr std::string_view suffix = ".disabled";
        if (!name.ends_with(suffix) || !std::string_view(name).substr(0, name.size() - suffix.size()).ends_with(".md")) {
            return path;
        }
        target = name.substr(0, name.size() - suffix.size());
    } else if (name.ends_with(".md")) {
        target = disabled_twin(path);
    } else {
        return path;
    }
    if (exists_no_follow(target)) {
        return std::unexpected(Error::already_exists(target));
    }
    std::error_code ec;
    stdfs::rename(path, target, ec);
    if (ec) {
        return std::unexpected(Error::io(path, io_error(ec)));
    }
    return target;
}

Result<> remove(const std::filesystem::path& path) {
    std::error_code ec;
    if (!stdfs::remove(path, ec)) {
        if (!ec) {
            ec = std::make_error_code(std::errc::no_such_file_or_directory);
        }
        return std::unexpected(Error::io(path, io_error(ec)));
    }
    return {};
}

} // namespace loa::settings::models::agents
