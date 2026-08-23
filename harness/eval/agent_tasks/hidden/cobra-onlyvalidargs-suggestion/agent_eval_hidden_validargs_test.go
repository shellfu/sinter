package cobra

import (
	"strings"
	"testing"
)

func TestAgentEvalOnlyValidArgsSuggestsOffending(t *testing.T) {
	c := &Command{Use: "c", Args: OnlyValidArgs, ValidArgs: []string{"alpha", "bravo"}, Run: emptyRun}
	err := OnlyValidArgs(c, []string{"alpha", "bravx"})
	if err == nil {
		t.Fatal("expected error")
	}
	msg := err.Error()
	if !strings.Contains(msg, `invalid argument "bravx"`) {
		t.Fatalf("unexpected message %q", msg)
	}
	if !strings.Contains(msg, "\tbravo") || strings.Contains(msg, "\talpha") {
		t.Fatalf("suggestions must target the offending arg: %q", msg)
	}
}
