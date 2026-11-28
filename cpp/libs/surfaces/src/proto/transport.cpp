#include "loa/surfaces/proto/transport.hpp"

#include <algorithm>
#include <cstring>

#include "loa/runtime/proto.hpp"
#include "mailbox.hpp"

namespace loa::surfaces::proto {

namespace {

// One direction of the in-memory duplex stream (tokio `duplex`), bounded like it.
struct Pipe {
    explicit Pipe(const asio::any_io_executor& ex) : readable(ex), writable(ex) {}

    static constexpr std::size_t CAPACITY = 256 * 1024;
    std::mutex mutex;
    std::deque<std::uint8_t> data;
    bool write_closed = false;
    bool read_closed = false;
    detail::Signal readable;
    detail::Signal writable;
};

class PipeReader final : public StreamReader {
  public:
    explicit PipeReader(std::shared_ptr<Pipe> pipe) : pipe_(std::move(pipe)) {}
    ~PipeReader() override {
        {
            std::lock_guard lock(pipe_->mutex);
            pipe_->read_closed = true;
            pipe_->data.clear();
        }
        pipe_->writable.notify();
    }

    Awaitable<Result<std::size_t>> read_some(std::span<std::uint8_t> into) override {
        for (;;) {
            {
                std::lock_guard lock(pipe_->mutex);
                if (!pipe_->data.empty()) {
                    const auto n = std::min(into.size(), pipe_->data.size());
                    std::copy_n(pipe_->data.begin(), n, into.begin());
                    pipe_->data.erase(pipe_->data.begin(), pipe_->data.begin() + static_cast<std::ptrdiff_t>(n));
                    pipe_->writable.notify();
                    co_return n;
                }
                if (pipe_->write_closed) co_return std::size_t{0};
            }
            if (!co_await pipe_->readable.wait()) co_return std::unexpected(ProtoError::io("read cancelled"));
        }
    }

  private:
    std::shared_ptr<Pipe> pipe_;
};

class PipeWriter final : public StreamWriter {
  public:
    explicit PipeWriter(std::shared_ptr<Pipe> pipe) : pipe_(std::move(pipe)) {}
    ~PipeWriter() override { close(); }

    Awaitable<Result<void>> write_all(std::span<const std::uint8_t> bytes) override {
        while (!bytes.empty()) {
            {
                std::lock_guard lock(pipe_->mutex);
                if (pipe_->read_closed) co_return std::unexpected(ProtoError::io("broken pipe"));
                if (pipe_->write_closed) co_return std::unexpected(ProtoError::io("write after shutdown"));
                const auto n = std::min(bytes.size(), Pipe::CAPACITY - pipe_->data.size());
                if (n > 0) {
                    pipe_->data.insert(pipe_->data.end(), bytes.begin(), bytes.begin() + static_cast<std::ptrdiff_t>(n));
                    bytes = bytes.subspan(n);
                    pipe_->readable.notify();
                    continue;
                }
            }
            if (!co_await pipe_->writable.wait()) co_return std::unexpected(ProtoError::io("write cancelled"));
        }
        co_return Result<void>{};
    }

    Awaitable<Result<void>> shutdown() override {
        close();
        co_return Result<void>{};
    }

  private:
    void close() {
        {
            std::lock_guard lock(pipe_->mutex);
            pipe_->write_closed = true;
        }
        pipe_->readable.notify();
    }

    std::shared_ptr<Pipe> pipe_;
};

using DatagramChannel = asio::experimental::concurrent_channel<void(asio::error_code, Bytes)>;

class MemoryDatagrams final : public Datagrams {
  public:
    MemoryDatagrams(std::optional<std::size_t> max, std::shared_ptr<DatagramChannel> tx, std::shared_ptr<DatagramChannel> rx)
        : max_(max), tx_(std::move(tx)), rx_(std::move(rx)) {}
    // Like dropping the tokio sender: the peer's `recv` ends with `Closed`.
    ~MemoryDatagrams() override { tx_->close(); }

    std::optional<std::size_t> max_size() const override { return max_; }

    std::expected<void, DatagramError> send(Bytes datagram) override {
        if (!max_) return std::unexpected(DatagramError::unsupported());
        if (datagram.size() > *max_) return std::unexpected(DatagramError::too_large(datagram.size(), *max_));
        if (!tx_->is_open()) return std::unexpected(DatagramError::closed());
        // A full queue drops the datagram silently.
        (void)tx_->try_send(asio::error_code{}, std::move(datagram));
        return {};
    }

    Awaitable<std::expected<Bytes, DatagramError>> recv() override {
        if (!max_) co_return std::unexpected(DatagramError::unsupported());
        auto [ec, bytes] = co_await rx_->async_receive(asio::as_tuple(asio::use_awaitable));
        if (ec) co_return std::unexpected(DatagramError::closed());
        co_return std::move(bytes);
    }

  private:
    std::optional<std::size_t> max_;
    std::shared_ptr<DatagramChannel> tx_;
    std::shared_ptr<DatagramChannel> rx_;
};

// Position just past a varint length prefix at the start of `bytes`, and its value.
struct Prefix {
    std::size_t size;
    std::uint64_t len;
};

std::optional<Prefix> length_prefix(std::span<const std::uint8_t> bytes) {
    std::uint64_t len = 0;
    for (std::size_t i = 0; i < std::min<std::size_t>(bytes.size(), 10); ++i) {
        len |= static_cast<std::uint64_t>(bytes[i] & 0x7F) << (7 * i);
        if ((bytes[i] & 0x80) == 0) return Prefix{i + 1, len};
    }
    return std::nullopt;
}

} // namespace

std::pair<Transport, Transport> memory_pair(const asio::any_io_executor& executor, std::optional<std::size_t> max_datagram) {
    auto a_to_b = std::make_shared<Pipe>(executor);
    auto b_to_a = std::make_shared<Pipe>(executor);
    auto a_tx = std::make_shared<DatagramChannel>(executor, 1024);
    auto b_tx = std::make_shared<DatagramChannel>(executor, 1024);
    Transport a{std::make_unique<PipeReader>(b_to_a), std::make_unique<PipeWriter>(a_to_b),
                std::make_shared<MemoryDatagrams>(max_datagram, a_tx, b_tx)};
    Transport b{std::make_unique<PipeReader>(a_to_b), std::make_unique<PipeWriter>(b_to_a),
                std::make_shared<MemoryDatagrams>(max_datagram, b_tx, a_tx)};
    return {std::move(a), std::move(b)};
}

Result<std::optional<StreamMessage>> MessageReader::buffered() {
    const std::span<const std::uint8_t> avail(buf_.data() + start_, end_ - start_);
    const auto prefix = length_prefix(avail);
    if (!prefix) {
        if (avail.size() >= 10) return std::unexpected(ProtoError::protocol("message length prefix is not a varint"));
        return std::optional<StreamMessage>{};
    }
    if (prefix->len > MAX_STREAM_MESSAGE) return std::unexpected(ProtoError::too_large(prefix->len, MAX_STREAM_MESSAGE));
    if (avail.size() < prefix->size + prefix->len) return std::optional<StreamMessage>{};
    auto msg = decode_stream_message(avail.subspan(prefix->size, prefix->len));
    start_ += prefix->size + prefix->len;
    if (!msg) return std::unexpected(ProtoError::decode(msg.error()));
    return std::optional<StreamMessage>(std::move(*msg));
}

Awaitable<Result<std::optional<StreamMessage>>> MessageReader::next() {
    for (;;) {
        auto ready = buffered();
        if (!ready || *ready) co_return ready;
        std::size_t wanted = 8 * 1024;
        if (auto prefix = length_prefix({buf_.data() + start_, end_ - start_}))
            wanted = std::max<std::size_t>(wanted, prefix->size + prefix->len - (end_ - start_));
        // Compact, then make room; bytes land only in `[end_, size)`, so a cancelled read loses nothing.
        if (start_ > 0) {
            std::memmove(buf_.data(), buf_.data() + start_, end_ - start_);
            end_ -= start_;
            start_ = 0;
        }
        if (buf_.size() - end_ < wanted) buf_.resize(end_ + wanted);
        auto n = co_await inner_->read_some({buf_.data() + end_, buf_.size() - end_});
        if (!n) co_return std::unexpected(std::move(n).error());
        if (*n == 0) {
            if (end_ == start_) co_return std::optional<StreamMessage>{};
            co_return std::unexpected(ProtoError::io("unexpected end of file"));
        }
        end_ += *n;
    }
}

Awaitable<Result<void>> MessageWriter::send(StreamMessage msg) {
    buf_.clear();
    encode_length_delimited(buf_, msg);
    co_return co_await inner_->write_all(buf_);
}

Awaitable<Result<void>> MessageWriter::finish() { co_return co_await inner_->shutdown(); }

} // namespace loa::surfaces::proto
