#pragma once

// Structured reads of a page for the agent: URL and title, readable text, and
// an outline of the interactive elements. Port of
// `loa-surfaces/src/browser/structured.rs`.

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::surfaces::browser {

// The current page location and the title shown in its browser tab.
struct PageInfo {
    std::string url;
    std::string title;

    [[nodiscard]] loa::runtime::json::Value to_json() const;
    friend bool operator==(const PageInfo&, const PageInfo&) = default;
};

// A box in surface coordinates (pixels from the top left of the surface).
// Parts outside the surface are scrolled out of view.
struct Bounds {
    float x = 0, y = 0, width = 0, height = 0;

    // The point a click on this element should go to.
    [[nodiscard]] std::pair<float, float> center() const { return {x + width / 2.0F, y + height / 2.0F}; }
    [[nodiscard]] loa::runtime::json::Value to_json() const;
    friend bool operator==(const Bounds&, const Bounds&) = default;
};

// One interactive element: its accessibility role and name and where it is.
struct OutlineNode {
    std::string role;
    std::string name;
    Bounds bounds;
    // Whether any part of it is on the surface right now.
    bool visible = false;

    [[nodiscard]] loa::runtime::json::Value to_json() const;
    friend bool operator==(const OutlineNode&, const OutlineNode&) = default;
};

// The outline from a `Accessibility.getFullAXTree` result's `nodes` and a
// `DOMSnapshot.captureSnapshot` result, for a `width` x `height` surface:
// interactive elements in document order, placed with the first document's
// layout boxes.
[[nodiscard]] std::vector<OutlineNode> outline_from(const loa::runtime::json::Value& ax_nodes,
                                                    const loa::runtime::json::Value& snapshot, std::uint32_t width,
                                                    std::uint32_t height);

} // namespace loa::surfaces::browser
