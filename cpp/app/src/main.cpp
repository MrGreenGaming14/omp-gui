// `loa-qt`: the Qt desktop app.

#include "loa/ui/app/application.hpp"

int main(int argc, char** argv) {
    loa::ui::app::Application app(argc, argv);
    return app.run();
}
