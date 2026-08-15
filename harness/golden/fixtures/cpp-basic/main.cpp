#include "util.h"
#include <cstdio>

// Greets the player by name.
void greet(const char *name) {
  printf("hello %s\n", name);
}

namespace math {
int square(int x) { return x * x; }
}

class Counter {
public:
  int bump() {
    greet("counter");
    return clamp(2, 0, 10);
  }
};
