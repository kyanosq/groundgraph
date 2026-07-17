#include "engine.h"

namespace eng {

// Cross-file consumer of Engine (declared in engine.h): a free function in
// the same namespace that constructs an Engine and calls power().
int run() {
    Engine e;
    return e.power();
}

}  // namespace eng
