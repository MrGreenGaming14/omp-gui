#include "loa/ui/session/decisions/card.hpp"

#include <algorithm>
#include <cctype>

#include "loa/runtime/json.hpp"

namespace loa::ui::session::decisions {

namespace json = loa::runtime::json;
namespace diff = loa::ui::panel::diff;
using loa::view::theme::Icon;

std::string text_field_id(NodeId decision) { return "decision-" + loa::core::to_string(decision) + "-text"; }
std::string form_field_id(NodeId decision, std::size_t index) {
    return "decision-" + loa::core::to_string(decision) + "-field-" + std::to_string(index);
}

namespace {

std::optional<Reply> one(std::string_view value, std::optional<std::string> reason) {
    return Reply{{std::string(value)}, std::move(reason)};
}

// Rust `str::lines().count()`.
std::size_t line_count(std::string_view s) {
    if (s.empty()) return 0;
    auto n = static_cast<std::size_t>(std::ranges::count(s, '\n'));
    return s.back() == '\n' ? n : n + 1;
}

// `body` parsed as a unified diff; `nullopt` when it is no parseable diff with lines.
std::optional<std::vector<diff::FileDiff>> parse_diff(std::string_view body) {
    auto files = diff::parse_unified(body);
    if (!files || files->empty()) return std::nullopt;
    if (std::ranges::none_of(*files, [](const auto& f) { return !f.lines().empty(); })) return std::nullopt;
    return std::move(*files);
}

bool ascii_ieq(std::string_view a, std::string_view b) {
    return a.size() == b.size() && std::ranges::equal(a, b, [](unsigned char x, unsigned char y) {
               return std::tolower(x) == std::tolower(y);
           });
}

class ApprovalKind final : public CardKind {
    static std::optional<Reply> reply(const Card& c, ApprovalChoice choice) {
        return one(answer_of(choice), choice == ApprovalChoice::Deny ? c.reason() : std::nullopt);
    }
    static std::optional<ApprovalChoice> resolve(const Decision& d, std::string_view value) {
        auto c = from_answer<ApprovalChoice>(value);
        if (c == ApprovalChoice::AllowSession && !d.allows_session()) return ApprovalChoice::AllowOnce;
        return c;
    }

  public:
    std::pair<Icon, std::string_view> header(const Decision& d) const override {
        return {Icon::TriangleAlert, d.tool ? "wants to run" : "needs approval"};
    }
    std::string_view notice() const override { return "needs approval"; }
    std::string_view settings_label() const override { return "Approvals"; }
    std::size_t options(const Card& c) const override { return c.decision.approval_choices().size(); }
    std::optional<Reply> choose(Card& c, std::size_t i) const override {
        const auto list = c.decision.approval_choices();
        if (i >= list.size()) return std::nullopt;
        return reply(c, list[i]);
    }
    std::optional<Reply> confirm(Card& c) const override {
        if (!c.selected) return std::nullopt;
        return choose(c, *c.selected);
    }
    std::optional<Reply> dismiss(Card& c) const override { return reply(c, ApprovalChoice::Deny); }
    std::optional<Reply> submit_text(Card& c) const override { return reply(c, ApprovalChoice::Deny); }
    bool valid_default(const Decision& d) const override {
        return d.defaults.size() == 1 && from_answer<ApprovalChoice>(d.defaults[0]).has_value();
    }
    std::optional<std::vector<std::string>> default_values(const Decision& d) const override {
        if (d.defaults.size() != 1) return std::nullopt;
        auto c = resolve(d, d.defaults[0]);
        if (!c) return std::nullopt;
        return std::vector<std::string>{std::string(answer_of(*c))};
    }
    Tone outcome(std::string_view first) const override {
        const auto c = from_answer<ApprovalChoice>(first);
        if (!c) return Tone::Normal;
        return *c == ApprovalChoice::Deny ? Tone::Danger : Tone::Ok;
    }
    std::optional<std::pair<std::string, bool>> policy(const Decision& d, std::string_view configured) const override {
        auto c = resolve(d, configured);
        if (!c) return std::nullopt;
        return std::pair{std::string(answer_of(*c)), *c == ApprovalChoice::Deny};
    }
    std::vector<std::pair<std::string_view, std::string_view>> presets() const override {
        std::vector<std::pair<std::string_view, std::string_view>> out;
        for (auto it = Choice<ApprovalChoice>::ALL.rbegin(); it != Choice<ApprovalChoice>::ALL.rend(); ++it)
            out.emplace_back(answer_of(*it), label_of(*it));
        return out;
    }
};

class QuestionKind final : public CardKind {
    static std::optional<Reply> typed(const Card& c) {
        auto r = c.reason();
        if (!r) return std::nullopt;
        return one(*r, std::nullopt);
    }

  public:
    std::pair<Icon, std::string_view> header(const Decision&) const override { return {Icon::CircleDot, "asks"}; }
    std::string_view notice() const override { return "has a question"; }
    std::string_view settings_label() const override { return "Questions"; }
    std::size_t options(const Card& c) const override { return c.decision.options.size(); }
    std::optional<Reply> choose(Card& c, std::size_t i) const override {
        if (!c.decision.multi) return one(c.decision.options[i], std::nullopt);
        c.picked[i] = !c.picked[i];
        c.selected = i;
        return std::nullopt;
    }
    std::optional<Reply> confirm(Card& c) const override {
        const auto& d = c.decision;
        if (d.multi && std::ranges::find(c.picked, true) != c.picked.end()) {
            Reply r;
            for (std::size_t i = 0; i < d.options.size() && i < c.picked.size(); ++i)
                if (c.picked[i]) r.values.push_back(d.options[i]);
            return r;
        }
        if (c.selected && !d.multi) return one(d.options[*c.selected], std::nullopt);
        return typed(c);
    }
    std::optional<Reply> dismiss(Card&) const override { return Reply{}; }
    std::optional<Reply> submit_text(Card& c) const override { return typed(c); }
    bool valid_default(const Decision& d) const override {
        if (d.defaults.empty()) return false;
        if (d.defaults.size() == 1) return true;
        return d.multi && std::ranges::all_of(d.defaults, [&](const auto& a) {
                   return std::ranges::find(d.options, a) != d.options.end();
               });
    }
    // An option label picks that option; anything else is the typed answer.
    std::optional<std::pair<std::string, bool>> policy(const Decision& d, std::string_view configured) const override {
        const auto it = std::ranges::find_if(d.options, [&](const auto& o) { return ascii_ieq(o, configured); });
        return std::pair{it != d.options.end() ? *it : std::string(configured), false};
    }
    std::vector<std::pair<std::string_view, std::string_view>> presets() const override { return {}; }
};

class PlanKind final : public CardKind {
    static std::optional<Reply> reply(Card& c, PlanChoice choice) {
        switch (choice) {
        case PlanChoice::Approve: return one(answer_of(choice), std::nullopt);
        case PlanChoice::Edit:
            c.editor = c.decision.body;
            c.selected = std::nullopt;
            return std::nullopt;
        case PlanChoice::Reject: return one(answer_of(choice), c.reason());
        }
        return std::nullopt;
    }

  public:
    std::pair<Icon, std::string_view> header(const Decision&) const override { return {Icon::FileText, "proposes a plan"}; }
    std::string_view notice() const override { return "has a plan to review"; }
    std::string_view settings_label() const override { return "Plan reviews"; }
    std::size_t options(const Card& c) const override { return c.editor ? 0 : Choice<PlanChoice>::ALL.size(); }
    std::optional<Reply> choose(Card& c, std::size_t i) const override { return reply(c, Choice<PlanChoice>::ALL[i]); }
    std::optional<Reply> confirm(Card& c) const override {
        if (c.editor) return one(answer_of(PlanChoice::Edit), *c.editor);
        if (!c.selected) return std::nullopt;
        return reply(c, Choice<PlanChoice>::ALL[*c.selected]);
    }
    std::optional<Reply> dismiss(Card& c) const override {
        if (c.editor) {
            c.editor = std::nullopt;
            return std::nullopt;
        }
        return reply(c, PlanChoice::Reject);
    }
    std::optional<Reply> submit_text(Card& c) const override { return reply(c, PlanChoice::Reject); }
    bool valid_default(const Decision& d) const override {
        if (d.defaults.size() != 1) return false;
        const auto c = from_answer<PlanChoice>(d.defaults[0]);
        return c && *c != PlanChoice::Edit;
    }
    Tone outcome(std::string_view first) const override {
        const auto c = from_answer<PlanChoice>(first);
        if (!c) return Tone::Normal;
        return *c == PlanChoice::Reject ? Tone::Danger : Tone::Ok;
    }
    std::optional<std::pair<std::string, bool>> policy(const Decision&, std::string_view configured) const override {
        const auto c = from_answer<PlanChoice>(configured);
        if (!c || *c == PlanChoice::Edit) return std::nullopt;
        return std::pair{std::string(configured), *c == PlanChoice::Reject};
    }
    std::vector<std::pair<std::string_view, std::string_view>> presets() const override {
        return {{answer_of(PlanChoice::Reject), label_of(PlanChoice::Reject)},
                {answer_of(PlanChoice::Approve), label_of(PlanChoice::Approve)}};
    }
};

class FormKind final : public CardKind {
  public:
    std::pair<Icon, std::string_view> header(const Decision&) const override { return {Icon::SlidersHorizontal, "needs input"}; }
    std::string_view notice() const override { return "needs a form filled in"; }
    std::string_view settings_label() const override { return "Forms"; }
    std::size_t options(const Card&) const override { return 0; }
    std::optional<Reply> choose(Card&, std::size_t) const override { return std::nullopt; }
    std::optional<Reply> confirm(Card& c) const override {
        if (!c.form || !*c.form) return std::nullopt;
        auto answer = (*c.form)->submit();
        if (!answer) return std::nullopt;
        return one(*answer, std::nullopt);
    }
    std::optional<Reply> dismiss(Card&) const override { return Reply{}; }
    std::optional<Reply> submit_text(Card&) const override { return std::nullopt; }
    bool valid_default(const Decision& d) const override {
        if (d.defaults.size() != 1) return false;
        auto v = json::parse(d.defaults[0]);
        return v && v->is_object();
    }
    // The form's declared defaults with the configured object's values over them.
    std::optional<std::pair<std::string, bool>> policy(const Decision& d, std::string_view configured) const override {
        auto overrides = json::parse(configured);
        if (!overrides || !overrides->is_object()) return std::nullopt;
        json::Value values = json::Object{};
        if (d.fields)
            if (auto form = Form::parse(*d.fields))
                if (auto submitted = form->submit())
                    if (auto parsed = json::parse(*submitted); parsed && parsed->is_object()) values = std::move(*parsed);
        for (auto& [key, value] : *overrides->as_object()) values.set(key, std::move(value));
        return std::pair{json::dump(values), false};
    }
    std::vector<std::pair<std::string_view, std::string_view>> presets() const override {
        return {{FORM_DEFAULTS, "Submit the form's defaults"}};
    }
};

} // namespace

const CardKind& kind_for(DecisionKind kind) {
    static const ApprovalKind approval;
    static const QuestionKind question;
    static const PlanKind plan;
    static const FormKind form;
    switch (kind) {
    case DecisionKind::Approval: return approval;
    case DecisionKind::Question: return question;
    case DecisionKind::Plan: return plan;
    case DecisionKind::Form: return form;
    }
    return approval;
}

Card::Card(Decision d) : decision(std::move(d)) { parse(true, true, true); }

void Card::parse(bool body, bool fields, bool options) {
    const auto& d = decision;
    if (body) {
        diff.reset();
        if (d.kind == DecisionKind::Approval)
            if (auto files = parse_diff(d.body)) diff.emplace(std::move(*files), line_count(d.body));
    }
    if (fields) {
        form.reset();
        if (d.fields && d.kind == DecisionKind::Form) form = Form::parse(*d.fields);
    }
    if (options) {
        picked.assign(d.options.size(), false);
        selected = std::nullopt;
    }
    if (!d.is_waiting()) {
        pending = false;
        editor = std::nullopt;
    }
}

void Card::refresh(Decision d) {
    if (d == decision) return;
    Decision old = std::exchange(decision, std::move(d));
    parse(old.body != decision.body, old.fields != decision.fields, old.options != decision.options);
}

const CardKind& Card::kind() const { return kind_for(decision.kind); }

void Card::pick(std::size_t index) {
    if (index >= kind().options(*this)) return;
    if (decision.multi && decision.kind == DecisionKind::Question) picked[index] = !picked[index];
    selected = index;
}

std::optional<Reply> Card::choose(std::size_t index) {
    if (index >= kind().options(*this)) return std::nullopt;
    return kind().choose(*this, index);
}

void Card::set_field(std::size_t index, const FieldInput& input) {
    if (!form || !*form) return;
    auto& f = **form;
    std::visit(
        [&](const auto& in) {
            using T = std::decay_t<decltype(in)>;
            if constexpr (std::is_same_v<T, field_input::Text>) f.set(index, form::value::Text{in.text});
            else if constexpr (std::is_same_v<T, field_input::Bool>) f.set(index, form::value::Bool{in.on});
            else if constexpr (std::is_same_v<T, field_input::Choice>) f.set(index, form::value::Choice{in.index});
            else f.toggle(index, in.option);
        },
        input);
}

std::optional<std::string> Card::reason() const {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto b = text.find_first_not_of(ws);
    if (b == std::string::npos) return std::nullopt;
    return text.substr(b, text.find_last_not_of(ws) - b + 1);
}

std::string_view node_name(std::string_view by, std::string_view local, std::optional<std::string_view> local_name) {
    if (local_name && !local_name->empty() && by == local) return *local_name;
    if (by.size() >= 32 && std::ranges::all_of(by, [](unsigned char b) { return std::isalnum(b) != 0; })) return by.substr(0, 8);
    return by;
}

Header header(const Card& c) {
    const auto& d = c.decision;
    const auto [icon, label] = c.kind().header(d);
    Header h{icon, d.harness.value_or("The agent") + " " + std::string(label), d.tool, std::nullopt};
    // An approval naming its tool says nothing more; other headers show the title.
    if (!(d.kind == DecisionKind::Approval && d.tool)) h.title = d.title;
    return h;
}

std::optional<Footer> footer(const Card& c, bool view_only, std::string_view local, std::optional<std::string_view> local_name) {
    const auto& d = c.decision;
    const auto by = [&](std::string prefix) {
        if (!d.by) return prefix;
        if (*d.by == TIMEOUT_BY) return prefix + " by timeout (default answer)";
        return prefix + " by " + std::string(node_name(*d.by, local, local_name));
    };
    Footer f;
    switch (d.status) {
    case DecisionStatus::Waiting:
        if (c.pending) f.status = "Sending answer…";
        else if (view_only) f.status = "View only: you can't answer this prompt";
        else return std::nullopt;
        break;
    case DecisionStatus::Answered:
        f.status = by(answer_summary(d.kind, d.answer));
        f.tone = d.answer.empty() ? Tone::Muted : c.kind().outcome(d.answer.front());
        if (d.kind == DecisionKind::Form && d.answer.size() == 1) f.json = d.answer.front();
        f.reason = d.reason;
        f.reason_mono = d.kind == DecisionKind::Plan && !d.answer.empty() &&
                        from_answer<PlanChoice>(d.answer.front()) == PlanChoice::Edit;
        break;
    case DecisionStatus::Expired: f.status = by("Expired"); break;
    case DecisionStatus::Cancelled: f.status = "Cancelled"; break;
    }
    return f;
}

std::vector<PreviewRow> preview_rows(const std::vector<diff::FileDiff>& files, bool full) {
    const std::size_t limit = full ? SIZE_MAX : PREVIEW_LINES;
    std::vector<PreviewRow> rows;
    for (const auto& file : files) {
        if (rows.size() >= limit) break;
        rows.push_back({true, diff::Tag::Equal, std::string(file.path())});
        const auto lines = file.lines();
        for (std::size_t i = 0; i < lines.size() && rows.size() < limit; ++i) {
            const char mark = lines[i].tag == diff::Tag::Insert ? '+' : lines[i].tag == diff::Tag::Delete ? '-' : ' ';
            rows.push_back({false, lines[i].tag, mark + std::string(file.line_text(i))});
        }
    }
    return rows;
}

std::optional<std::string> preview_toggle(const Card& c) {
    if (!c.diff || c.diff->second <= PREVIEW_LINES) return std::nullopt;
    if (c.show_full) return "Show fewer lines";
    return "Show all " + std::to_string(c.diff->second) + " lines";
}

} // namespace loa::ui::session::decisions
