#pragma once

// Deterministic XML export of a session DOM for debugging (export only).
// Elements carry `nid` then attributes sorted by name; non-string values add a
// `.int`/`.float`/`.bool`/`.bytes`/`.list` suffix (bytes as base64, lists as
// `[1, 'str', bytes(..)]`); odd name characters become `_x<hex>_`; text is
// escaped verbatim before any children, which indent by two spaces.

#include <string>

#include "loa/core/dom.hpp"

namespace loa::core::xml {

// The XML export of `dom` as a string.
[[nodiscard]] std::string to_string(const Dom& dom);
// Appends the XML export of `dom` to `out`.
void write(const Dom& dom, std::string& out);

} // namespace loa::core::xml
