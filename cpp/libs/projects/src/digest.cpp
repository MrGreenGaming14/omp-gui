#include "loa/projects/digest.hpp"

#include <algorithm>
#include <limits>

#include "loa/core/schema.hpp"

namespace loa::projects {

namespace attr = core::attr;
using core::ElementKind;
using core::Value;
using settings::DigestContent;
using settings::DigestState;
using settings::FileNote;

namespace {

const core::Node* digest_node(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return nullptr;
    auto digests = dom.children_of_kind(root->id, ElementKind::Digest);
    return digests.empty() ? nullptr : digests.back();
}

std::uint64_t uint(const core::Node& node, std::string_view name) {
    return static_cast<std::uint64_t>(std::max<std::int64_t>(node.int_attr(name).value_or(0), 0));
}

bool is_stale(const core::Dom& dom, const core::Node& node) {
    if (node.bool_attr(attr::STALE) == true) return true;
    const auto seq = uint(node, attr::SOURCE_SEQ);
    const auto own = uint(node, OWN_PATCHES);
    const auto covered = seq > std::numeric_limits<std::uint64_t>::max() - own ? std::numeric_limits<std::uint64_t>::max() : seq + own;
    return dom.seq() > covered;
}

Value int_value(std::uint64_t n) {
    return Value(static_cast<std::int64_t>(std::min<std::uint64_t>(n, std::numeric_limits<std::int64_t>::max())));
}

Value strings(const std::vector<std::string>& items) {
    Value::List list;
    for (const auto& s : items) list.emplace_back(s);
    return Value(std::move(list));
}

} // namespace

std::optional<Digest> digest_of(const core::Dom& dom) {
    const auto* node = digest_node(dom);
    if (!node) return std::nullopt;
    const auto list = [&](std::string_view name) {
        std::vector<std::string> out;
        if (const auto* v = node->attr(name))
            for (auto s : v->str_items()) out.emplace_back(s);
        return out;
    };
    DigestContent content;
    content.summary = node->text;
    content.decisions = list(attr::DECISIONS);
    for (const auto& f : list(attr::FILES)) content.files.push_back(FileNote::parse(f));
    content.open_items = list(attr::OPEN_ITEMS);
    if (const auto* v = node->attr(attr::POINTERS))
        if (auto items = v->as_list())
            for (const auto& item : *items)
                if (auto id = item.as_int()) content.pointers.push_back(core::NodeId{static_cast<std::uint64_t>(std::max<std::int64_t>(*id, 0))});
    std::optional<std::string> model;
    if (auto m = node->str_attr(attr::MODEL)) model = std::string(*m);
    return Digest{node->id, std::move(content), uint(*node, attr::SOURCE_SEQ), is_stale(dom, *node), std::move(model)};
}

DigestState digest_state(const core::Dom& dom) {
    const auto* node = digest_node(dom);
    if (!node) return DigestState::Missing;
    return is_stale(dom, *node) ? DigestState::Stale : DigestState::Fresh;
}

std::vector<core::Draft> write_drafts(const core::Dom& dom, const DigestContent& content, std::uint64_t source_seq,
                                      std::optional<std::string_view> model, std::int64_t created) {
    const auto* root = dom.root();
    if (!root) return {};
    std::vector<core::Draft> drafts;
    for (const auto* d : dom.children_of_kind(root->id, ElementKind::Digest)) drafts.push_back(core::Draft::remove(d->id));
    std::vector<std::string> files;
    for (const auto& f : content.files) files.push_back(f.encode());
    Value::List pointers;
    for (const auto& p : content.pointers) pointers.push_back(int_value(p.value));
    core::Attrs attrs;
    attrs.insert_or_assign(std::string(attr::SOURCE_SEQ), int_value(source_seq));
    attrs.insert_or_assign(std::string(OWN_PATCHES), int_value(drafts.size() + 1));
    attrs.insert_or_assign(std::string(attr::DECISIONS), strings(content.decisions));
    attrs.insert_or_assign(std::string(attr::FILES), strings(files));
    attrs.insert_or_assign(std::string(attr::OPEN_ITEMS), strings(content.open_items));
    attrs.insert_or_assign(std::string(attr::POINTERS), Value(std::move(pointers)));
    attrs.insert_or_assign(std::string(attr::CREATED), Value(created));
    if (model) attrs.insert_or_assign(std::string(attr::MODEL), Value(*model));
    drafts.push_back(core::Draft::insert(root->id, ElementKind::Digest, std::move(attrs), content.summary));
    return drafts;
}

std::optional<core::Draft> mark_stale_draft(const core::Dom& dom) {
    const auto* node = digest_node(dom);
    if (!node || node->bool_attr(attr::STALE) == true) return std::nullopt;
    return core::Draft::set(node->id, std::string(attr::STALE), true);
}

} // namespace loa::projects
