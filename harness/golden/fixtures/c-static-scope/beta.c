// File-scope helper, private to beta.c.
static int helper(void) { return 2; }

int beta_run(void) {
  return helper();
}
