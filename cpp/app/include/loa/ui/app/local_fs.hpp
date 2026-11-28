#pragma once

// The local disk behind the files pane's `FileSystem` port: honours `.gitignore`, `.ignore` and
// git excludes (negation, `**`, nested files), shows dotfiles and hides `.git`. Port of
// `loa_ui_panel::files::LocalFs`; it lives in the app so UI modules never touch the disk.

#include "loa/view/panel.hpp"

namespace loa::ui::app {

class LocalFs final : public loa::view::panel::port::FileSystem {
  public:
    [[nodiscard]] std::expected<std::vector<loa::view::panel::port::Entry>, std::string> list(const std::filesystem::path& dir) const override;
};

} // namespace loa::ui::app
