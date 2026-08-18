// File-scope helper, private to alpha.c.
static int helper(void) { return 1; }

int alpha_run(void) {
  return helper();
}
