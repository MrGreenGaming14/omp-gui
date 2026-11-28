#pragma once

// The sidebar's lookup over the `NodeStatus` list the app hands to the shell. Port of
// `loa-ui-shell/src/data.rs`.

#include <string_view>
#include <utility>
#include <vector>

#include "loa/view/shell.hpp"

namespace loa::ui::shell {

class Nodes {
  public:
    Nodes() = default;
    explicit Nodes(std::vector<loa::view::shell::NodeStatus> list) : list_(std::move(list)) {}

    [[nodiscard]] const loa::view::shell::NodeStatus* get(std::string_view key) const;
    // The node's name, or the first eight characters of an unknown key.
    [[nodiscard]] std::string_view name_of(std::string_view key) const;
    [[nodiscard]] bool is_online(std::string_view key) const;
    [[nodiscard]] const std::vector<loa::view::shell::NodeStatus>& list() const { return list_; }

    friend bool operator==(const Nodes&, const Nodes&) = default;

  private:
    std::vector<loa::view::shell::NodeStatus> list_;
};

} // namespace loa::ui::shell
