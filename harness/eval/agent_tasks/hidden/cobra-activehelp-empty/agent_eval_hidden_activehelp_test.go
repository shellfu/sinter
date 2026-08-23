package cobra

import "testing"

func TestAgentEvalAppendActiveHelpEmpty(t *testing.T) {
	comps := []string{"a"}
	got := AppendActiveHelp(comps, "")
	if len(got) != 1 {
		t.Fatalf("expected no-op, got %v", got)
	}
	got = AppendActiveHelp(comps, "hint")
	if len(got) != 2 || got[1] != activeHelpMarker+"hint" {
		t.Fatalf("expected hint appended, got %v", got)
	}
}
