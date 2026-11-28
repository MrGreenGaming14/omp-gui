#pragma once

#include "loa/ui/kit/theme.hpp"

namespace loa::ui::kit::theme {

// The shared builtin token set of `v`.
std::shared_ptr<const TokenSet> builtin_set(Variant v);

} // namespace loa::ui::kit::theme
