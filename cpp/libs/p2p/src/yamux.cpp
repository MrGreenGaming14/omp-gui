#include "loa/p2p/yamux.hpp"

#include <algorithm>
#include <condition_variable>
#include <deque>
#include <format>
#include <map>
#include <mutex>
#include <thread>

namespace loa::p2p::yamux {

std::array<std::uint8_t, HEADER_SIZE> encode(const Header& h) {
    return {h.version,
            static_cast<std::uint8_t>(h.tag),
            static_cast<std::uint8_t>(h.flags >> 8),
            static_cast<std::uint8_t>(h.flags),
            static_cast<std::uint8_t>(h.stream_id >> 24),
            static_cast<std::uint8_t>(h.stream_id >> 16),
            static_cast<std::uint8_t>(h.stream_id >> 8),
            static_cast<std::uint8_t>(h.stream_id),
            static_cast<std::uint8_t>(h.length >> 24),
            static_cast<std::uint8_t>(h.length >> 16),
            static_cast<std::uint8_t>(h.length >> 8),
            static_cast<std::uint8_t>(h.length)};
}

Result<Header> decode(std::span<const std::uint8_t, HEADER_SIZE> b) {
    if (b[0] != 0) return fail(NodeError::Kind::decode, std::format("unknown yamux version {}", b[0]));
    if (b[1] > 3) return fail(NodeError::Kind::decode, std::format("unknown yamux frame type {}", b[1]));
    const auto be32 = [&](std::size_t i) {
        return std::uint32_t{b[i]} << 24 | std::uint32_t{b[i + 1]} << 16 | std::uint32_t{b[i + 2]} << 8 | b[i + 3];
    };
    return Header{0, static_cast<Tag>(b[1]), static_cast<std::uint16_t>(b[2] << 8 | b[3]), be32(4), be32(8)};
}

namespace {

struct StreamState {
    explicit StreamState(std::uint32_t i, std::uint32_t send) : id(i), send_window(send) {}
    std::uint32_t id;
    std::uint32_t send_window;
    std::uint32_t recv_window = DEFAULT_CREDIT; // credit the peer still has
    std::uint32_t consumed = 0;                 // read by the application, not yet credited back
    std::deque<std::uint8_t> buf;
    bool recv_closed = false;
    bool send_closed = false;
    bool reset = false;
};

Bytes frame(const Header& h, std::span<const std::uint8_t> body = {}) {
    const auto head = encode(h);
    Bytes out(head.begin(), head.end());
    out.insert(out.end(), body.begin(), body.end());
    return out;
}

} // namespace

struct Session::Impl : std::enable_shared_from_this<Impl> {
    Impl(std::shared_ptr<Socket> s, Mode md, Config c)
        : socket(std::move(s)), mode(md), config(c), next_id(md == Mode::client ? 1 : 2) {}

    std::shared_ptr<Socket> socket;
    Mode mode;
    Config config;
    std::mutex m;
    std::condition_variable cv;
    std::map<std::uint32_t, std::shared_ptr<StreamState>> streams;
    std::deque<std::shared_ptr<StreamState>> inbound;
    std::deque<Bytes> out;
    std::uint32_t next_id;
    bool ended = false;
    std::string reason;

    // Callers hold `m`.
    void enqueue(Bytes bytes) {
        out.push_back(std::move(bytes));
        cv.notify_all();
    }
    void end(std::string why) {
        if (ended) return;
        ended = true;
        reason = std::move(why);
        for (auto& [_, st] : streams) st->recv_closed = st->send_closed = true;
        streams.clear();
        cv.notify_all();
    }
    void terminate(std::uint32_t code, std::string why) {
        if (ended) return;
        enqueue(frame(Header::go_away(code)));
        end(std::move(why));
    }
    void forget_if_done(const std::shared_ptr<StreamState>& st) {
        if (st->reset || (st->send_closed && st->recv_closed)) {
            if (auto it = streams.find(st->id); it != streams.end() && it->second == st) streams.erase(it);
        }
    }
    bool valid_remote(std::uint32_t id) const {
        return id != 0 && (mode == Mode::client ? id % 2 == 0 : id % 2 == 1);
    }

    void writer() {
        std::unique_lock lock(m);
        for (;;) {
            cv.wait(lock, [&] { return !out.empty() || ended; });
            if (out.empty()) break;
            auto bytes = std::move(out.front());
            out.pop_front();
            lock.unlock();
            auto ok = socket->write_all(bytes);
            lock.lock();
            if (!ok) {
                out.clear();
                end(ok.error().to_string());
                break;
            }
        }
        lock.unlock();
        socket->shutdown();
    }

    Result<bool> read_exact(std::span<std::uint8_t> buf) {
        bool any = false;
        while (!buf.empty()) {
            auto n = socket->read(buf);
            if (!n) return std::unexpected(n.error());
            if (*n == 0) {
                if (!any) return false;
                return fail(NodeError::Kind::io, "early eof");
            }
            any = true;
            buf = buf.subspan(*n);
        }
        return true;
    }

    void reader() {
        std::array<std::uint8_t, HEADER_SIZE> head{};
        for (;;) {
            auto got = read_exact(head);
            if (!got || !*got) {
                std::lock_guard lock(m);
                end(got ? "peer closed the connection" : got.error().to_string());
                return;
            }
            auto header = decode(head);
            if (!header) {
                std::lock_guard lock(m);
                terminate(GO_AWAY_PROTOCOL_ERROR, header.error().to_string());
                return;
            }
            Bytes body;
            if (header->tag == Tag::data && header->length > 0) {
                if (header->length > MAX_FRAME_BODY) {
                    std::lock_guard lock(m);
                    terminate(GO_AWAY_PROTOCOL_ERROR, "yamux frame body too large");
                    return;
                }
                body.resize(header->length);
                auto full = read_exact(body);
                if (!full || !*full) {
                    std::lock_guard lock(m);
                    end(full ? "early eof" : full.error().to_string());
                    return;
                }
            }
            std::lock_guard lock(m);
            if (ended) return;
            if (!on_frame(*header, std::move(body))) return;
        }
    }

    // Holds `m`; false once the connection ended.
    bool on_frame(const Header& h, Bytes body) {
        switch (h.tag) {
        case Tag::go_away: end("connection is closed"); return false;
        case Tag::ping:
            if (!(h.flags & ACK) && (h.stream_id == 0 || streams.contains(h.stream_id)))
                enqueue(frame(Header::ping(h.length, ACK)));
            return true;
        case Tag::data:
        case Tag::window_update: break;
        }
        const bool data = h.tag == Tag::data;
        const auto id = h.stream_id;
        auto it = streams.find(id);
        if (h.flags & RST) {
            if (it != streams.end()) {
                it->second->reset = true;
                streams.erase(it);
                cv.notify_all();
            }
            return true;
        }
        const bool fin = h.flags & FIN;
        if (h.flags & SYN) {
            if (!valid_remote(id) || it != streams.end()) {
                terminate(GO_AWAY_PROTOCOL_ERROR, std::format("invalid stream id {}", id));
                return false;
            }
            if (streams.size() >= config.max_num_streams) {
                terminate(data ? GO_AWAY_INTERNAL_ERROR : GO_AWAY_PROTOCOL_ERROR, "maximum number of streams reached");
                return false;
            }
            std::uint32_t send = DEFAULT_CREDIT;
            if (data) {
                if (body.size() > DEFAULT_CREDIT) {
                    terminate(GO_AWAY_PROTOCOL_ERROR, "1st body of stream exceeds default credit");
                    return false;
                }
            } else {
                if (h.length > UINT32_MAX - DEFAULT_CREDIT) {
                    terminate(GO_AWAY_PROTOCOL_ERROR, "header contains invalid credit");
                    return false;
                }
                send += h.length;
            }
            auto st = std::make_shared<StreamState>(id, send);
            st->recv_window -= static_cast<std::uint32_t>(body.size());
            st->buf.assign(body.begin(), body.end());
            st->recv_closed = fin;
            streams.emplace(id, st);
            inbound.push_back(st);
            // Acknowledge at once so the peer's backlog of unacknowledged streams drains.
            enqueue(frame(Header::window_update(id, 0, ACK)));
            cv.notify_all();
            return true;
        }
        if (it == streams.end()) return true; // possibly dropped earlier
        auto& st = *it->second;
        if (data) {
            if (body.size() > st.recv_window) {
                terminate(GO_AWAY_PROTOCOL_ERROR, "frame body larger than window of stream");
                return false;
            }
            st.recv_window -= static_cast<std::uint32_t>(body.size());
            st.buf.insert(st.buf.end(), body.begin(), body.end());
        } else {
            if (h.length > UINT32_MAX - st.send_window) {
                terminate(GO_AWAY_PROTOCOL_ERROR, "could not increase the send window");
                return false;
            }
            st.send_window += h.length;
        }
        if (fin) st.recv_closed = true;
        forget_if_done(it->second);
        cv.notify_all();
        return true;
    }
};

namespace {

using Impl = Session::Impl;

// Both halves of one stream; the last half dropped closes what is still open.
struct Handle {
    Handle(std::shared_ptr<Impl> s, std::shared_ptr<StreamState> t) : session(std::move(s)), st(std::move(t)) {}
    ~Handle() {
        std::lock_guard lock(session->m);
        if (!session->ended && !st->reset && !(st->send_closed && st->recv_closed)) {
            if (!st->send_closed && !st->recv_closed)
                session->enqueue(frame(Header::data(st->id, 0, RST)));
            else if (!st->send_closed)
                session->enqueue(frame(Header::data(st->id, 0, FIN)));
        }
        st->send_closed = st->recv_closed = true;
        session->forget_if_done(st);
    }
    std::shared_ptr<Impl> session;
    std::shared_ptr<StreamState> st;
};

class YamuxSend final : public SendHalf {
  public:
    explicit YamuxSend(std::shared_ptr<Handle> h) : h_(std::move(h)) {}

    Result<> write_all(std::span<const std::uint8_t> data) override {
        auto& s = *h_->session;
        auto& st = *h_->st;
        std::unique_lock lock(s.m);
        while (!data.empty()) {
            s.cv.wait(lock, [&] { return st.send_window > 0 || st.reset || st.send_closed || s.ended; });
            if (s.ended) return fail(NodeError::Kind::io, "connection is closed");
            if (st.reset) return fail(NodeError::Kind::io, "stream reset");
            if (st.send_closed) return fail(NodeError::Kind::io, "write after shutdown");
            const auto k = std::min({static_cast<std::size_t>(st.send_window), data.size(), s.config.split_send_size});
            st.send_window -= static_cast<std::uint32_t>(k);
            s.enqueue(frame(Header::data(st.id, static_cast<std::uint32_t>(k)), data.first(k)));
            data = data.subspan(k);
        }
        return {};
    }

    Result<> shutdown() override {
        auto& s = *h_->session;
        std::lock_guard lock(s.m);
        auto& st = h_->st;
        if (st->send_closed || st->reset) return {};
        st->send_closed = true;
        if (!s.ended) s.enqueue(frame(Header::data(st->id, 0, FIN)));
        s.forget_if_done(st);
        return {};
    }

  private:
    std::shared_ptr<Handle> h_;
};

class YamuxRecv final : public RecvHalf {
  public:
    explicit YamuxRecv(std::shared_ptr<Handle> h) : h_(std::move(h)) {}

    Result<std::size_t> read(std::span<std::uint8_t> buf) override {
        if (buf.empty()) return std::size_t{0};
        auto& s = *h_->session;
        auto& st = *h_->st;
        std::unique_lock lock(s.m);
        s.cv.wait(lock, [&] { return !st.buf.empty() || st.recv_closed || st.reset || s.ended; });
        if (!st.buf.empty()) {
            const auto n = std::min(buf.size(), st.buf.size());
            std::copy_n(st.buf.begin(), n, buf.begin());
            st.buf.erase(st.buf.begin(), st.buf.begin() + static_cast<std::ptrdiff_t>(n));
            st.consumed += static_cast<std::uint32_t>(n);
            if (st.consumed >= DEFAULT_CREDIT / 2 && !st.recv_closed && !s.ended) {
                s.enqueue(frame(Header::window_update(st.id, st.consumed)));
                st.recv_window += st.consumed;
                st.consumed = 0;
            }
            return n;
        }
        if (st.reset) return fail(NodeError::Kind::io, "stream reset");
        return std::size_t{0};
    }

  private:
    std::shared_ptr<Handle> h_;
};

Stream make_stream(std::shared_ptr<Impl> s, std::shared_ptr<StreamState> st) {
    auto h = std::make_shared<Handle>(std::move(s), std::move(st));
    return Stream{std::make_unique<YamuxSend>(h), std::make_unique<YamuxRecv>(h)};
}

} // namespace

Session::Session(std::shared_ptr<Impl> impl) : impl_(std::move(impl)) {}

Session::~Session() { close("connection handles dropped"); }

std::shared_ptr<Session> Session::start(std::shared_ptr<Socket> socket, Mode mode, Config config) {
    auto impl = std::make_shared<Impl>(std::move(socket), mode, config);
    std::thread([impl] { impl->reader(); }).detach();
    std::thread([impl] { impl->writer(); }).detach();
    return std::shared_ptr<Session>(new Session(std::move(impl)));
}

Result<Stream> Session::open() {
    std::lock_guard lock(impl_->m);
    if (impl_->ended) return fail(NodeError::Kind::net, "connection is closed");
    if (impl_->streams.size() >= impl_->config.max_num_streams)
        return fail(NodeError::Kind::net, "maximum number of streams reached");
    if (impl_->next_id > UINT32_MAX - 2) return fail(NodeError::Kind::net, "number of stream ids has been exhausted");
    const auto id = impl_->next_id;
    impl_->next_id += 2;
    auto st = std::make_shared<StreamState>(id, DEFAULT_CREDIT);
    impl_->streams.emplace(id, st);
    impl_->enqueue(frame(Header::window_update(id, 0, SYN)));
    return make_stream(impl_, std::move(st));
}

Result<Stream> Session::accept() {
    std::unique_lock lock(impl_->m);
    impl_->cv.wait(lock, [&] { return !impl_->inbound.empty() || impl_->ended; });
    if (impl_->inbound.empty()) return fail(NodeError::Kind::net, impl_->reason);
    auto st = std::move(impl_->inbound.front());
    impl_->inbound.pop_front();
    lock.unlock();
    return make_stream(impl_, std::move(st));
}

void Session::close(std::string reason) {
    std::lock_guard lock(impl_->m);
    impl_->terminate(GO_AWAY_NORMAL, std::move(reason));
}

std::string Session::closed() {
    std::unique_lock lock(impl_->m);
    impl_->cv.wait(lock, [&] { return impl_->ended; });
    return impl_->reason;
}

} // namespace loa::p2p::yamux
