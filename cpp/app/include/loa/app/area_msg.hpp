#pragma once

// Any area's message behind `msg::Area` (port of `AreaMsg` in `loa-app/src/area.rs`): a
// type-checked erased value replacing Rust's `Box<dyn Erased>`. Copying clones the value;
// `downcast` hands it back only as the exact type it was made from.

#include <memory>
#include <optional>
#include <string>
#include <typeinfo>
#include <utility>

namespace loa::app {

class AreaMsg {
  public:
    template <typename M>
    [[nodiscard]] static AreaMsg make(M message) {
        return AreaMsg(std::make_unique<Held<M>>(std::move(message)));
    }

    AreaMsg(const AreaMsg& other) : value_(other.value_ ? other.value_->clone() : nullptr) {}
    AreaMsg(AreaMsg&&) noexcept = default;
    AreaMsg& operator=(const AreaMsg& other) {
        if (this != &other) value_ = other.value_ ? other.value_->clone() : nullptr;
        return *this;
    }
    AreaMsg& operator=(AreaMsg&&) noexcept = default;
    ~AreaMsg() = default;

    // Whether the message is an `M`.
    template <typename M>
    [[nodiscard]] bool is() const noexcept {
        return value_ && value_->type() == typeid(M);
    }

    // The message as `M`; `nullptr` when it is another type.
    template <typename M>
    [[nodiscard]] const M* downcast_ref() const noexcept {
        return is<M>() ? &static_cast<const Held<M>*>(value_.get())->value : nullptr;
    }

    // The message as `M` (moved out), or `nullopt` leaving it untouched when it is another type.
    template <typename M>
    [[nodiscard]] std::optional<M> downcast() && {
        if (!is<M>()) return std::nullopt;
        return std::move(static_cast<Held<M>*>(value_.get())->value);
    }

    // The held type's name, for notices (Rust `Debug`).
    [[nodiscard]] std::string type_name() const { return value_ ? value_->type().name() : "nothing"; }

  private:
    struct Base {
        virtual ~Base() = default;
        [[nodiscard]] virtual std::unique_ptr<Base> clone() const = 0;
        [[nodiscard]] virtual const std::type_info& type() const noexcept = 0;
    };
    template <typename M>
    struct Held final : Base {
        explicit Held(M m) : value(std::move(m)) {}
        [[nodiscard]] std::unique_ptr<Base> clone() const override { return std::make_unique<Held>(value); }
        [[nodiscard]] const std::type_info& type() const noexcept override { return typeid(M); }
        M value;
    };

    explicit AreaMsg(std::unique_ptr<Base> value) : value_(std::move(value)) {}
    std::unique_ptr<Base> value_;
};

} // namespace loa::app
