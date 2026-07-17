#include "core.h"

// Declaration in core.h, definition here — the canonical C split. C has one
// flat namespace and a separate-translation-unit model, so cross-TU calls
// (and same-TU calls like buffer_size -> buffer_round) rely on the C spec's
// module-wide resolution.
int buffer_size(struct Buffer *b) {
    return buffer_round(b->len);
}

int buffer_round(int n) {
    return n + 1;
}
