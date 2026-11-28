#include "loa/ui/app/asio_bridge.hpp"

#include <mutex>

#include <asio.hpp>

#include <QCoreApplication>
#include <QMetaObject>
#include <QObject>
#include <QThread>

namespace loa::ui::app {

struct AsioBridge::State {
    std::mutex lock;
    QObject* context = nullptr; // owned; lives in the GUI thread

    void post(std::function<void()> fn) {
        std::scoped_lock g(lock);
        // Qt drops events still queued for a deleted receiver, so a late completion is harmless.
        if (context) QMetaObject::invokeMethod(context, std::move(fn), Qt::QueuedConnection);
    }
};

AsioBridge::AsioBridge(loa::runtime::Executor& executor, QObject* gui) : executor_(executor), state_(std::make_shared<State>()) {
    state_->context = new QObject;
    if (gui) state_->context->moveToThread(gui->thread());
}

AsioBridge::~AsioBridge() {
    QObject* ctx = nullptr;
    {
        std::scoped_lock g(state_->lock);
        std::swap(ctx, state_->context);
    }
    delete ctx;
}

void AsioBridge::post(std::function<void()> fn) const { state_->post(std::move(fn)); }

void AsioBridge::dispatch(std::function<void()> fn) const { asio::post(executor_.context(), std::move(fn)); }

void AsioBridge::spawn(loa::runtime::Task task, std::function<void(std::exception_ptr)> done) {
    executor_.spawn([](loa::runtime::Task t, std::shared_ptr<State> state,
                       std::function<void(std::exception_ptr)> done) -> loa::runtime::Task {
        std::exception_ptr error;
        try {
            co_await std::move(t);
        } catch (...) {
            error = std::current_exception();
        }
        if (done) state->post([done = std::move(done), error] { done(error); });
    }(std::move(task), state_, std::move(done)));
}

} // namespace loa::ui::app
