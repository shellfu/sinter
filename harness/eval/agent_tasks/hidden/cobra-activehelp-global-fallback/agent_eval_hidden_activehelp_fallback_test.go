package cobra

import "testing"

func TestAgentEvalActiveHelpGlobalFallback(t *testing.T) {
	root := &Command{Use: "prog", Run: emptyRun}
	t.Setenv("PROG_ACTIVE_HELP", "")
	_ = activeHelpEnvVar
	t.Setenv("COBRA_ACTIVE_HELP", "1")
	if got := GetActiveHelpConfig(root); got != "1" {
		t.Fatalf("expected fallback to global value 1, got %q", got)
	}
	t.Setenv("PROG_ACTIVE_HELP", "2")
	if got := GetActiveHelpConfig(root); got != "2" {
		t.Fatalf("expected program value 2, got %q", got)
	}
	t.Setenv("COBRA_ACTIVE_HELP", "0")
	if got := GetActiveHelpConfig(root); got != "0" {
		t.Fatalf("expected global disable, got %q", got)
	}
}
