#include "bar.h"

// Maximum number of retries.
#define MAX_RETRIES 3
// Squares a value.
#define SQUARE(x) ((x) * (x))

// A 2D point.
struct Point {
  int x;
  int y;
};

// Supported colors.
enum Color { RED, GREEN, BLUE };

// Either an int or a float.
union Value {
  int i;
  float f;
};

// A point by another name.
typedef struct Point Coord;

void foo_run(void);

/* Entry point: greets via bar. */
void foo_run(void) {
  bar_greet("foo");
}
