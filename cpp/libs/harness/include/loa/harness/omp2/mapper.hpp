#pragma once

#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <variant>
#include <vector>

#include "loa/harness/kit/mapper.hpp"
#include "loa/harness/omp2/wire.hpp"

namespace loa::harness::omp2 {

inline constexpr std::string_view HANDLE_ATTR = "x-omp2-handle";

class Omp2Mapper final : public kit::EventMapper<wire::Input, wire::Reply> {
  public:
    explicit Omp2Mapper(core::NodeId root);
    [[nodiscard]] static Omp2Mapper from_dom(const core::Dom& dom);
    [[nodiscard]] contracts::Result<core::NodeId> begin(const core::DomWriter& w, const contracts::Prompt& prompt);
    [[nodiscard]] contracts::Result<> finish(const core::DomWriter& w, core::TurnStatus status);
    contracts::Result<std::vector<wire::Reply>> apply(wire::Input input, const core::DomWriter& w) override;
    contracts::Result<std::optional<wire::Reply>> answer(core::NodeId decision, const contracts::Answer& answer,
                                                         const core::DomWriter& w) override;
    [[nodiscard]] const kit::TurnLedger<std::string>& ledger() const override { return ledger_; }
    kit::TurnLedger<std::string>& ledger_mut() override { return ledger_; }

  private:
    enum class Element { Turn, User, Block, Call, Input, Result, Usage, Todo, Job, Subagent, Compaction };
    enum class Rule { Root, Flatten, Assistant, Element, Ext };
    enum class TargetKind { Unset, Root, Flat, Assistant, Node };
    struct Target {
        TargetKind kind = TargetKind::Unset;
        core::NodeId id{};
        std::optional<Element> element;
    };
    struct Node {
        wire::Tag tag;
        std::optional<wire::Handle> parent;
        std::vector<wire::Handle> kids;
        std::vector<std::pair<std::string, wire::Json>> props;
        std::optional<std::string> content;
        Target target;
        explicit Node(wire::NodeSpec spec, std::optional<wire::Handle> parent);
        [[nodiscard]] const wire::Json* prop(std::string_view name) const;
    };
    struct TextStream { core::NodeId id; };
    struct PropStream { wire::Handle handle; std::string prop; std::string text; };
    struct IgnoredStream {};
    using Stream = std::variant<TextStream, PropStream, IgnoredStream>;
    struct Approval { std::string prompt_id; bool allow_session; };
    struct Select { wire::Json id; std::vector<std::string> options; };
    using Pending = std::variant<Approval, Select>;
    using Handle = wire::Handle;
    using Id = core::NodeId;
    using Result = contracts::Result<>;
    struct ResolvedRule { Rule rule; std::optional<Element> element; };
    [[nodiscard]] static core::ElementKind kind(Element e);
    [[nodiscard]] static ResolvedRule rule(const wire::Tag& tag, const wire::Tag& parent_tag,
                                           std::optional<core::ElementKind> parent);
    [[nodiscard]] static std::optional<std::string_view> dest(Element e, std::string_view prop);
    [[nodiscard]] static std::optional<std::string_view> enum_value(Element e, std::string_view name,
                                                                     std::string_view raw);
    [[nodiscard]] static core::Attrs fixed_attrs(Element e, const wire::Tag& tag);
    [[nodiscard]] static std::pair<std::string, std::optional<core::Value>> element_write(
        Element e, std::string_view prop, const wire::Json& v);

    explicit Omp2Mapper(kit::TurnLedger<std::string> ledger);
    [[nodiscard]] std::optional<Id> running_turn();
    void turn_seen(Id id);
    [[nodiscard]] Result frame(const core::DomWriter& w, wire::Frame frame);
    [[nodiscard]] Result decide(const core::DomWriter& w, std::optional<Id> call, core::Attrs attrs,
                                std::string text, Pending pending);
    [[nodiscard]] Result dom(const core::DomWriter& w, wire::DomEvent event);
    [[nodiscard]] contracts::Result<const Node*> node(Handle h) const;
    [[nodiscard]] const std::vector<Handle>& kids(Handle h) const;
    [[nodiscard]] std::optional<Target> target(Handle h) const;
    [[nodiscard]] bool flat(Handle h) const;
    void set_target(Handle h, Target target);
    void bind(Handle h, Id id, Id parent);
    [[nodiscard]] contracts::Result<std::pair<Id, std::optional<core::ElementKind>>> app_parent_of(Handle h) const;
    [[nodiscard]] std::optional<std::uint32_t> app_index(Handle h) const;
    void app_nodes(Handle h, std::vector<Id>& out) const;
    [[nodiscard]] std::optional<std::uint32_t> turn_index() const;
    [[nodiscard]] bool under(Id id, const std::unordered_set<Id>& set) const;
    void forget(const std::unordered_set<Id>& gone);
    [[nodiscard]] Result detach(const core::DomWriter& w, Handle h);
    [[nodiscard]] Result link(Handle h, Handle parent, std::optional<Handle> after);
    [[nodiscard]] Result op(const core::DomWriter& w, wire::Op op);
    [[nodiscard]] Result set_prop(const core::DomWriter& w, Handle h, std::string prop, wire::Json value);
    [[nodiscard]] contracts::Result<ResolvedRule> rule_of(Handle h) const;
    [[nodiscard]] Result materialize(const core::DomWriter& w, Handle h, bool live);
    [[nodiscard]] Result insert_element(const core::DomWriter& w, Handle h, Element e, bool live);
    [[nodiscard]] contracts::Result<std::optional<Id>> bind_pending(const core::DomWriter& w, Handle h, Element e, Id parent);
    [[nodiscard]] Result insert_ext(const core::DomWriter& w, Handle h);
    [[nodiscard]] Result write(const core::DomWriter& w, Id id, Element e, std::string_view prop, const wire::Json& v);
    [[nodiscard]] std::optional<Id> result_of(Id call) const;
    [[nodiscard]] Result on_set(const core::DomWriter& w, Handle h, std::string_view prop, const wire::Json& value);
    [[nodiscard]] Result assistant_set(const core::DomWriter& w, Handle h, std::string_view prop, const wire::Json& value);
    [[nodiscard]] Result stream(const core::DomWriter& w, std::uint32_t sid, wire::StreamOp op,
                                std::optional<std::pair<Handle, std::string>> bound, std::optional<std::string> text);
    [[nodiscard]] contracts::Result<Stream> stream_target(Handle h, std::string_view prop,
                                                          std::optional<std::string_view> text) const;
    [[nodiscard]] Result reset(const core::DomWriter& w, wire::Snapshot snapshot);
    [[nodiscard]] Result reconcile(const core::DomWriter& w, Handle h);

    kit::TurnLedger<std::string> ledger_;
    std::uint64_t high_water_ = 0;
    std::map<Handle, Node> tree_;
    std::map<Handle, Id> ids_;
    std::unordered_map<Id, std::pair<Handle, Id>> back_;
    std::unordered_map<std::uint32_t, Stream> streams_;
    std::map<std::pair<Handle, std::string>, std::string> seed_;
    std::optional<std::pair<Id, std::optional<Id>>> pending_;
    std::optional<Id> turn_;
    std::map<Id, bool> open_;
    std::unordered_map<std::string, Id> calls_;
    std::unordered_map<Id, Pending> decisions_;
};

} // namespace loa::harness::omp2
