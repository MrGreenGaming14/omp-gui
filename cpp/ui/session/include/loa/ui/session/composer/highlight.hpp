#pragma once

// Colours in the editor (port of `composer/highlight.rs` and `composer/shimmer.rs`): a
// `/command` that starts the message, `@` file mentions, `[Image #n]` tokens and omp's magic
// keywords, drawn in a gradient that shimmers while the phase advances. The shimmer clock has
// `PHASES` steps per 1.8 s cycle, read from wall-clock time so everything moves in step.

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/ui/kit/theme.hpp"

namespace loa::ui::session::composer {

namespace shimmer {
// Steps of the shimmer per cycle.
inline constexpr std::uint8_t PHASES = 56;
// One shimmer cycle, as in omp.
inline constexpr std::int64_t CYCLE_MS = 1800;
// Ticks that reach every step, for the keyword gradient.
inline constexpr std::chrono::milliseconds EVERY_STEP{CYCLE_MS / PHASES};

// The step at `now_ms`, Unix milliseconds.
[[nodiscard]] std::uint8_t phase_at(std::int64_t now_ms);
// How far `phase` is through its cycle, `0.0..1.0`.
[[nodiscard]] float progress(std::uint8_t phase);
// The wall-clock time in Unix milliseconds.
[[nodiscard]] std::int64_t now_ms();
} // namespace shimmer

namespace highlight {
struct Command {
    friend bool operator==(const Command&, const Command&) = default;
};
struct Mention {
    friend bool operator==(const Mention&, const Mention&) = default;
};
struct Image {
    std::uint32_t n = 0;
    friend bool operator==(const Image&, const Image&) = default;
};
struct Keyword {
    kit::theme::Color color;
    friend bool operator==(const Keyword&, const Keyword&) = default;
};
} // namespace highlight

using Highlight = std::variant<highlight::Command, highlight::Mention, highlight::Image, highlight::Keyword>;

// One highlighted byte range of a line.
struct Span {
    std::size_t begin = 0, end = 0;
    Highlight highlight;
    friend bool operator==(const Span&, const Span&) = default;
};

// How a highlight is drawn.
struct Format {
    kit::theme::Color color;
    bool bold = false;
    friend bool operator==(const Format&, const Format&) = default;
};

// The colour of image `n`'s token and card.
[[nodiscard]] kit::theme::Color image_color(std::uint32_t n);
// The text standing for pasted image `n` in the message.
[[nodiscard]] std::string image_token(std::uint32_t n);
// How `h` is drawn with tokens `t`.
[[nodiscard]] Format format(const Highlight& h, const kit::theme::TokenSet& t);
// Whether `line` holds a magic keyword, which makes the shimmer run.
[[nodiscard]] bool has_keyword(std::string_view line);
// The highlights of one line, in order and without overlaps. `first` marks the message's first
// line, where a `/command` may start it.
[[nodiscard]] std::vector<Span> spans(std::string_view line, bool first, std::uint8_t phase);
// The colour of character `index` of `chars` in keyword `keyword` (index into omp's list) at
// shimmer `phase`.
[[nodiscard]] kit::theme::Color gradient(std::size_t keyword, std::size_t index, std::size_t chars, std::uint8_t phase);
// HSL to RGB, every component `0..1` except `hue` in degrees.
[[nodiscard]] kit::theme::Color hsl(float hue, float saturation, float lightness);

// The editor's highlighter settings.
struct Settings {
    // Shimmer step, `0..shimmer::PHASES`.
    std::uint8_t phase = 0;
    friend bool operator==(const Settings&, const Settings&) = default;
};

// Highlights the editor's lines in order; a phase change recolours from the top.
class Highlighter {
  public:
    explicit Highlighter(Settings settings) : phase_(settings.phase) {}

    void update(Settings settings);
    void change_line(std::size_t line) { line_ = line_ < line ? line_ : line; }
    [[nodiscard]] std::vector<Span> highlight_line(std::string_view line);
    [[nodiscard]] std::size_t current_line() const { return line_; }
    [[nodiscard]] std::uint8_t phase() const { return phase_; }

  private:
    std::uint8_t phase_;
    std::size_t line_ = 0;
};

} // namespace loa::ui::session::composer
