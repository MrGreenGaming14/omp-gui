// `loa-qt`: the Qt desktop app (port of `loa-app/src/main.rs`).

#include <string>
#include <string_view>
#include <vector>

#include "loa/mcp/bridge.hpp"
#include "loa/ui/app/application.hpp"

int main(int argc, char** argv) {
    // `loa-qt omp-bridge …` serves omp's tools over stdio MCP to another harness; sessions
    // launch the app binary this way.
    if (argc > 1 && std::string_view(argv[1]) == "omp-bridge") {
        const std::vector<std::string> args(argv + 2, argv + argc);
        return loa::mcp::bridge::run_cli(args);
    }
    loa::ui::app::Application app(argc, argv);
    return app.run();
}
