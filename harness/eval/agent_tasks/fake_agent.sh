#!/bin/sh
# Dry-run stand-in for an agent CLI. Emits a claude-style stream-json
# transcript (one rg call, one sinter call, one fallback grep) and applies the
# fix for task sinter-rel-display-curdir so the validate step can pass.
# Runs with cwd = fresh clone; SINTER_EVAL_EXPECTED_FILE names the target file.
set -e
emit() { printf '%s\n' "$1"; }
emit '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"rg -n \"fn rel_display\" crates"}}]}}'
emit '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"crates/sinter-core/src/paths.rs:8:pub fn rel_display(path: &Path) -> String {"}]}}'
emit '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"sinter show rel_display"}}]}}'
emit "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t2\",\"content\":\"$(sinter show rel_display 2>/dev/null | head -c 600 | tr -d '"\\' | tr '\n' ' ')\"}]}}"
emit '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Read","input":{"file_path":"crates/sinter-core/src/paths.rs"}}]}}'
emit "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t3\",\"content\":\"$(wc -c < "$SINTER_EVAL_EXPECTED_FILE" | tr -d ' ') bytes\"}]}}"
sed -i 's|for comp in path.components() {|for comp in path.components().filter(\|c\| !matches!(c, std::path::Component::CurDir)) {|' "$SINTER_EVAL_EXPECTED_FILE"
emit '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t4","name":"Edit","input":{"file_path":"crates/sinter-core/src/paths.rs"}}]}}'
emit '{"type":"result","subtype":"success"}'
