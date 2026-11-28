#pragma once

// `JsonRpc2`: the JSON-RPC 2.0 envelope (acp, codex app-server).

#include "loa/harness/kit/rpc.hpp"

namespace loa::harness::kit {

// JSON-RPC "internal error", for failures that carry no code.
inline constexpr std::int64_t JSONRPC_INTERNAL = -32603;

// JSON-RPC 2.0 framing. `header` writes `"jsonrpc":"2.0"`; codex omits it.
class JsonRpc2 final : public Framing {
  public:
    explicit JsonRpc2(bool header = true) : header(header) {}

    bool header;

    [[nodiscard]] Json request(const Json& id, std::string_view method, Json params) const override;
    [[nodiscard]] Json notification(std::string_view method, Json params) const override;
    [[nodiscard]] Json response(Json id, std::string_view method, RpcResult<Json> result) const override;
    [[nodiscard]] std::optional<Incoming> classify(Json msg) const override;

  private:
    [[nodiscard]] Json frame(Json msg) const;
};

} // namespace loa::harness::kit
