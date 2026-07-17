#pragma once

namespace eng {

// Inline definitions so the methods live inside the class body — the parser
// then unambiguously tags them CppMethod (an out-of-line `Engine::power`
// definition in the .cpp is far harder to attach to the class). `power`
// calls `boost`, a same-file Calls edge that does not depend on cross-TU
// resolution.
class Engine {
public:
    int power() {
        return boost() + 1;
    }

    int boost() {
        return 10;
    }
};

}  // namespace eng
