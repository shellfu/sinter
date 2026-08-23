package cobra

import "testing"

func TestAgentEvalPluralArgs(t *testing.T) {
	c := &Command{Use: "c", Run: emptyRun}
	cases := []struct {
		v    PositionalArgs
		args []string
		want string
	}{
		{MinimumNArgs(1), []string{}, "requires at least 1 arg, only received 0"},
		{MinimumNArgs(2), []string{"a"}, "requires at least 2 args, only received 1"},
		{MaximumNArgs(1), []string{"a", "b"}, "accepts at most 1 arg, received 2"},
		{ExactArgs(1), []string{}, "accepts 1 arg, received 0"},
		{ExactArgs(2), []string{"a"}, "accepts 2 args, received 1"},
		{RangeArgs(2, 4), []string{"a"}, "accepts between 2 and 4 args, received 1"},
	}
	for _, tc := range cases {
		err := tc.v(c, tc.args)
		if err == nil || err.Error() != tc.want {
			t.Errorf("want %q, got %v", tc.want, err)
		}
	}
}
