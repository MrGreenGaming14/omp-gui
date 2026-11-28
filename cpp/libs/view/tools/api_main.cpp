// `loa_view_api <include/loa/view> <api.txt>`: the API gate CTest runs. Set
// `UPDATE_VIEW_API=1` to rewrite `api.txt` after bumping `loa::view::VERSION`.

#include <cstdlib>
#include <fstream>
#include <iostream>
#include <iterator>
#include <string>
#include <string_view>

#include "api_listing.hpp"
#include "loa/view/version.hpp"

int main(int argc, char** argv) {
    if (argc != 3) {
        std::cerr << "usage: loa_view_api <include/loa/view> <api.txt>\n";
        return 2;
    }
    std::string saved;
    if (std::ifstream in(argv[2], std::ios::binary); in) saved.assign(std::istreambuf_iterator<char>(in), {});
    const auto rendered = loa::view::api::render(argv[1]);
    const char* update = std::getenv("UPDATE_VIEW_API");
    const auto result = loa::view::api::gate(saved, rendered, loa::view::VERSION, update && std::string_view(update) == "1");
    if (result.write) {
        std::ofstream(argv[2], std::ios::binary) << "version = " << loa::view::VERSION << '\n' << rendered;
        std::cout << "wrote " << argv[2] << '\n';
    }
    if (!result.ok) std::cerr << result.message;
    return result.ok ? 0 : 1;
}
