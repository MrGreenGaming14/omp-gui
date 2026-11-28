// Agents and background jobs pane state. Port of `crates/loa-ui-panel/src/agents.rs`.

#include "loa/ui/panel/agents.hpp"

#include <algorithm>

namespace loa::ui::panel::agents {

namespace core = loa::core;
namespace attr = loa::core::attr;
namespace intent = loa::view::panel::agents_intent;

namespace {

constexpr std::array<std::string_view, 5> ATTRS{attr::ID, attr::TITLE, attr::COMMAND, attr::AGENT, attr::SESSION};

std::size_t count_newlines(std::string_view s) { return static_cast<std::size_t>(std::ranges::count(s, '\n')); }

std::string_view trim(std::string_view s) {
    constexpr std::string_view WS = " \t\r\n\f\v";
    const auto b = s.find_first_not_of(WS);
    if (b == std::string_view::npos) return {};
    return s.substr(b, s.find_last_not_of(WS) - b + 1);
}

std::optional<std::string_view> non_empty(const std::optional<std::string>& s) {
    if (!s || s->empty()) return std::nullopt;
    return std::string_view(*s);
}

} // namespace

std::optional<Job> Job::from_node(const core::Node& node) {
    if (node.kind != core::ElementKind::Job) return std::nullopt;
    const auto k = node.str_attr(attr::KIND);
    if (!k) return std::nullopt;
    const auto kind = core::parse<JobKind>(*k);
    if (!kind) return std::nullopt;
    Job job;
    job.node_ = node.id;
    job.kind_ = *kind;
    job.output_ = node.text;
    job.newlines_ = count_newlines(job.output_);
    job.read_attrs(node);
    return job;
}

void Job::read_attrs(const core::Node& node) {
    if (auto k = node.str_attr(attr::KIND))
        if (auto kind = core::parse<JobKind>(*k)) kind_ = *kind;
    status_ = JobStatus::Running;
    if (auto s = node.str_attr(attr::STATUS))
        if (auto st = core::parse<JobStatus>(*s)) status_ = *st;
    for (std::size_t i = 0; i < ATTRS.size(); ++i) {
        const auto v = node.str_attr(ATTRS[i]);
        attrs_[i] = v ? std::optional<std::string>(std::string(*v)) : std::nullopt;
    }
    started_ = node.int_attr(attr::STARTED);
    ended_ = node.int_attr(attr::ENDED);
    exit_code_ = node.int_attr(attr::EXIT_CODE);
}

std::optional<std::string_view> Job::id() const { return non_empty(attrs_[0]); }
std::optional<std::string_view> Job::session() const { return non_empty(attrs_[4]); }

std::string_view Job::label() const {
    for (const std::size_t i : {1, 3, 2})
        if (attrs_[i]) return *attrs_[i];
    return core::as_str(kind_);
}

std::string_view Job::last_line() const {
    std::string_view rest = output_;
    while (true) {
        const auto nl = rest.rfind('\n');
        const auto line = trim(nl == std::string_view::npos ? rest : rest.substr(nl + 1));
        if (!line.empty()) return line;
        if (nl == std::string_view::npos) return {};
        rest = rest.substr(0, nl);
    }
}

std::size_t Job::tail_start() const {
    std::string_view body = output_;
    if (body.ends_with('\n')) body.remove_suffix(1);
    std::size_t end = body.size();
    for (std::size_t i = 0; i < TAIL_LINES; ++i) {
        const auto nl = body.substr(0, end).rfind('\n');
        if (nl == std::string_view::npos) return 0;
        end = nl;
    }
    return end + 1;
}

std::size_t Job::line_count() const {
    return newlines_ + ((!output_.empty() && !output_.ends_with('\n')) ? 1 : 0);
}

void Job::set_output(std::string_view text) {
    output_.assign(text);
    newlines_ = count_newlines(text);
}

void Job::append_output(std::string_view chunk) {
    output_.append(chunk);
    newlines_ += count_newlines(chunk);
}

bool is_active_wait(const core::Node& n) {
    if (n.kind != core::ElementKind::Call || n.str_attr(attr::TOOL) != std::optional(WAIT_TOOL)) return false;
    const auto s = n.str_attr(attr::STATUS);
    if (!s) return false;
    const auto st = core::parse<core::CallStatus>(*s);
    return st == core::CallStatus::Pending || st == core::CallStatus::Running;
}

State State::from_dom(const core::Dom& dom, PaneKind kind) {
    State s(kind);
    s.reset(dom);
    return s;
}

bool State::accepts(JobKind kind) const noexcept {
    return kind_ == PaneKind::Agents ? kind == JobKind::Subagent : kind == JobKind::Shell;
}

void State::reset(const core::Dom& dom) {
    std::set<NodeId> expanded;
    for (const auto& [n, j] : jobs_)
        if (j.expanded_) expanded.insert(n);
    jobs_.clear();
    waits_.clear();
    for (const auto& node : dom.iter()) {
        if (auto job = Job::from_node(node); job && accepts(job->kind_)) {
            job->expanded_ = expanded.contains(job->node_);
            jobs_.emplace(job->node_, std::move(*job));
        }
        if (is_active_wait(node)) waits_.insert(node.id);
    }
    std::erase_if(full_, [&](NodeId n) { return !jobs_.contains(n); });
}

void State::refresh_wait(const core::Node& node) {
    if (is_active_wait(node)) waits_.insert(node.id);
    else waits_.erase(node.id);
}

void State::apply(const core::Dom& dom, const core::Patch& patch) {
    const NodeId node = patch.node;
    const core::Node* n = dom.get(node);
    const auto it = jobs_.find(node);
    Job* job = it == jobs_.end() ? nullptr : &it->second;
    if (std::holds_alternative<core::op::Insert>(patch.op)) {
        if (!n) return;
        if (auto j = Job::from_node(*n)) {
            if (accepts(j->kind_)) jobs_.insert_or_assign(node, std::move(*j));
        } else {
            refresh_wait(*n);
        }
    } else if (const auto* set = std::get_if<core::op::Set>(&patch.op)) {
        if (!n) return;
        if (job && set->name == core::TEXT) {
            job->set_output(n->text);
        } else if (job) {
            job->read_attrs(*n);
            if (!accepts(job->kind_)) {
                jobs_.erase(it);
                full_.erase(node);
            }
        } else {
            refresh_wait(*n);
        }
    } else if (const auto* app = std::get_if<core::op::Append>(&patch.op)) {
        if (job) job->append_output(app->text);
    } else {
        std::erase_if(jobs_, [&](const auto& kv) { return !dom.contains(kv.first); });
        std::erase_if(waits_, [&](NodeId id) { return !dom.contains(id); });
        std::erase_if(full_, [&](NodeId id) { return !dom.contains(id); });
    }
}

std::pair<Task, std::optional<Event>> State::update(Message message) {
    std::optional<Event> event;
    if (const auto* m = std::get_if<msg::Open>(&message)) {
        if (const auto* j = job(m->node); j && j->kind_ == JobKind::Subagent)
            if (auto id = j->id()) event = intent::OpenSubagent{std::string(*id)};
    } else if (const auto* m = std::get_if<msg::Detach>(&message)) {
        if (const auto* j = job(m->node); j && j->kind_ == JobKind::Subagent)
            if (auto s = j->session()) event = intent::DetachTranscript{std::string(*s)};
    } else if (const auto* m = std::get_if<msg::Cancel>(&message)) {
        if (const auto* j = job(m->node); j && j->running()) event = intent::Cancel{m->node, j->attrs_[0]};
    } else if (const auto* m = std::get_if<msg::Tick>(&message)) {
        now_ = m->now;
    } else if (const auto* m = std::get_if<msg::Toggle>(&message)) {
        if (auto it = jobs_.find(m->node); it != jobs_.end()) it->second.expanded_ = !it->second.expanded_;
    } else if (const auto* m = std::get_if<msg::ToggleFull>(&message)) {
        if (!full_.erase(m->node) && jobs_.contains(m->node)) full_.insert(m->node);
    }
    return {Task::none(), std::move(event)};
}

const Job* State::job(NodeId node) const {
    const auto it = jobs_.find(node);
    return it == jobs_.end() ? nullptr : &it->second;
}

bool State::is_awaited(NodeId node) const {
    const auto* j = job(node);
    return waiting() && j && j->running();
}

std::optional<std::int64_t> State::elapsed_ms(NodeId node) const {
    const auto* j = job(node);
    if (!j) return std::nullopt;
    auto end = j->ended_;
    if (!end && j->running()) end = now_;
    if (!end || !j->started_) return std::nullopt;
    return std::max<std::int64_t>(*end - *j->started_, 0);
}

} // namespace loa::ui::panel::agents
