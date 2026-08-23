package cobra

import "testing"

func TestAgentEvalDepth(t *testing.T) {
	root := &Command{Use: "root", Run: emptyRun}
	child := &Command{Use: "child", Run: emptyRun}
	grand := &Command{Use: "grand", Run: emptyRun}
	root.AddCommand(child)
	child.AddCommand(grand)
	if root.Depth() != 0 || child.Depth() != 1 || grand.Depth() != 2 {
		t.Fatalf("depths: %d %d %d", root.Depth(), child.Depth(), grand.Depth())
	}
}
