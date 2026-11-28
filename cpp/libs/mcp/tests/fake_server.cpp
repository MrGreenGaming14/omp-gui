// The fake MCP server on stdio, one JSON-RPC message per line.

#include <iostream>
#include <string>

#include "fake.hpp"

int main() {
    namespace json = loa::runtime::json;
    fake::Fake server;
    auto send = [](const json::Value& message) { std::cout << json::dump(message) << '\n' << std::flush; };
    std::string line;
    while (std::getline(std::cin, line)) {
        auto message = json::parse(line);
        if (!message) continue;
        if (auto reply = server.handle(*message, send)) send(*reply);
    }
    return 0;
}
