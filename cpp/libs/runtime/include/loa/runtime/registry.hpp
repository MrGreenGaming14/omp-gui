#pragma once

// A string-keyed registry of shared strategy objects, usually abstract classes.

#include <cstddef>
#include <map>
#include <memory>
#include <string>
#include <string_view>

namespace loa::runtime {

// Strategies keyed by a stable id, iterated in id order.
template <typename A>
class Registry {
  public:
    using Map = std::map<std::string, std::shared_ptr<A>, std::less<>>;

    // Adds `a` under `id`, replacing any earlier entry with that id (Rust `register`, a C++ keyword).
    void insert(std::string id, std::shared_ptr<A> a) { by_id_.insert_or_assign(std::move(id), std::move(a)); }

    [[nodiscard]] std::shared_ptr<A> get(std::string_view id) const {
        const auto it = by_id_.find(id);
        return it == by_id_.end() ? nullptr : it->second;
    }

    [[nodiscard]] typename Map::const_iterator begin() const noexcept { return by_id_.begin(); }
    [[nodiscard]] typename Map::const_iterator end() const noexcept { return by_id_.end(); }
    [[nodiscard]] std::size_t size() const noexcept { return by_id_.size(); }
    [[nodiscard]] bool empty() const noexcept { return by_id_.empty(); }

  private:
    Map by_id_;
};

} // namespace loa::runtime
