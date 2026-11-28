#pragma once

// Stable presentation API between the app and the UI modules. UI modules read
// the plain view models of `loa::view` and emit each screen's `Intent`; the app
// projects domain state into views and maps intents back. The public surface is
// versioned by `VERSION` and snapshotted in `cpp/libs/view/api.txt`.

#include <string_view>

namespace loa::view {

// Semver version of this presentation API. Bump it whenever `api.txt` changes:
// major for removals and changes, minor for additions.
inline constexpr std::string_view VERSION = "4.0.0";

} // namespace loa::view
