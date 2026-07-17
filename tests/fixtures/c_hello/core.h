#ifndef CORE_H
#define CORE_H

struct Buffer {
    int len;
    int cap;
};

int buffer_size(struct Buffer *b);
int buffer_round(int n);

#endif  // CORE_H
