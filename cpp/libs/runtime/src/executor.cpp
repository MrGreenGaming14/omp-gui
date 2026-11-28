#include "loa/runtime/executor.hpp"

#include <asio.hpp>

#include <atomic>
#include <exception>
#include <optional>
#include <thread>
#include <vector>

#include "loa/runtime/log.hpp"

namespace loa::runtime {

struct Executor::Impl {
    asio::io_context ctx;
    std::optional<asio::executor_work_guard<asio::io_context::executor_type>> guard{ctx.get_executor()};
    std::atomic<bool> stopped{false};
    std::vector<std::jthread> workers;
};

Executor::Executor(std::size_t workers) : impl_(std::make_unique<Impl>()) {
    impl_->workers.reserve(workers);
    for (std::size_t i = 0; i < workers; ++i) impl_->workers.emplace_back([ctx = &impl_->ctx] { ctx->run(); });
}

Executor::~Executor() {
    stop();
    impl_->workers.clear(); // joins
}

void Executor::run() {
    if (impl_->stopped.load()) return;
    impl_->ctx.run();
}

void Executor::stop() {
    if (impl_->stopped.exchange(true)) return;
    asio::post(impl_->ctx, [impl = impl_.get()] { impl->guard.reset(); });
    impl_->ctx.stop();
}

bool Executor::stopped() const noexcept { return impl_->stopped.load(); }

void Executor::spawn(Task task) {
    asio::co_spawn(impl_->ctx, std::move(task), [](std::exception_ptr e) {
        if (!e) return;
        try {
            std::rethrow_exception(e);
        } catch (const std::exception& ex) {
            log::error("loa::runtime::executor", std::string("spawned task failed: ") + ex.what());
        } catch (...) {
            log::error("loa::runtime::executor", "spawned task failed");
        }
    });
}

asio::io_context& Executor::context() noexcept { return impl_->ctx; }

} // namespace loa::runtime
