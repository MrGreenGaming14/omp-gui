#include "loa/ui/session/transcript/selection.hpp"

#include <algorithm>

namespace loa::ui::session::transcript {

std::size_t Selection::add(std::string text, std::vector<Link> links) {
    blocks_.push_back(Block{std::move(text), std::move(links)});
    return blocks_.size() - 1;
}

void Selection::press(Endpoint at, float x, float y) {
    clear();
    if (at.block >= blocks_.size()) return;
    at.offset = std::min(at.offset, blocks_[at.block].text.size());
    std::optional<std::string> link;
    for (const auto& l : blocks_[at.block].links)
        if (at.offset >= l.begin && at.offset < l.end) link = l.url;
    anchor_ = end_ = at;
    press_ = Press{x, y, std::move(link), false, at.block};
}

void Selection::drag(std::optional<Endpoint> at, float x, float y) {
    if (!press_ || !anchor_) return;
    const float dx = x - press_->x, dy = y - press_->y;
    press_->dragged |= (at && at->block != anchor_->block) || dx * dx + dy * dy >= 9.0F;
    if (press_->dragged && at && at->block < blocks_.size()) {
        at->offset = std::min(at->offset, blocks_[at->block].text.size());
        end_ = at;
    }
}

std::optional<std::string> Selection::release() {
    if (!press_) return std::nullopt;
    auto p = std::move(*press_);
    press_.reset();
    if (p.dragged) return std::nullopt;
    anchor_.reset();
    end_.reset();
    return p.link;
}

void Selection::clear() {
    anchor_.reset();
    end_.reset();
    press_.reset();
}

std::optional<std::pair<std::size_t, std::size_t>> Selection::range(std::size_t block) const {
    if (empty()) return std::nullopt;
    const auto lo = std::min(*anchor_, *end_), hi = std::max(*anchor_, *end_);
    if (block < lo.block || block > hi.block || block >= blocks_.size()) return std::nullopt;
    const auto begin = lo.block == block ? lo.offset : 0;
    const auto end = hi.block == block ? hi.offset : blocks_[block].text.size();
    if (begin >= end) return std::nullopt;
    return std::pair{begin, end};
}

std::string Selection::copy() const {
    std::string out;
    for (std::size_t b = 0; b < blocks_.size(); ++b) {
        const auto r = range(b);
        if (!r) continue;
        if (!out.empty()) out += "\n\n";
        out += std::string_view(blocks_[b].text).substr(r->first, r->second - r->first);
    }
    return out;
}

} // namespace loa::ui::session::transcript
