#!/usr/bin/env python3
"""Optional-use coding-agent evaluation for Sinter.

Drives an agent CLI over real change tasks (agent_tasks/tasks.json) in three
arms and measures whether having Sinter available changes task outcome and
discovery cost. Stdlib only. See README.md "Coding-agent evaluation".

  python3 harness/eval/agent_eval.py --agent claude --runs 1
  python3 harness/eval/agent_eval.py --dry-run            # fake agent, one task, one arm
"""
import argparse
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
WORKSPACE = HERE.parent.parent
TASKS = HERE / "agent_tasks" / "tasks.json"
HIDDEN = HERE / "agent_tasks" / "hidden"
AGENTS = HERE / "agent_tasks" / "agents.json"
SKILL_CARD = WORKSPACE / "crates" / "sinter-cli" / "skill" / "SKILL.md"
ARMS = ["baseline", "sinter-optional", "sinter-context"]
SINTER_BUDGET_BYTES = 8192  # per-response cap the skill card promises; gate 4

DISCOVERY = re.compile(
    r"""(?:^|[\s;|&(`'"])(rg|grep|egrep|fgrep|find|cat|head|tail|sed|awk|ls|sinter)(?=\s|$)"""
)


def sh(cmd, cwd, env=None, timeout=None, capture=True):
    return subprocess.run(
        cmd, cwd=cwd, env=env, shell=isinstance(cmd, str), timeout=timeout,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.STDOUT if capture else None, text=True,
    )


# ---------------------------------------------------------------- workspace
def clone(repo, dst):
    url = str(WORKSPACE) if repo["url"] == "self" else repo["url"]
    sh(["git", "init", "-q", str(dst)], cwd=None, capture=True)
    sh(["git", "remote", "add", "origin", url], cwd=dst)
    r = sh(["git", "fetch", "-q", "--depth", "1", "origin", repo["commit"]], cwd=dst)
    if r.returncode:
        raise RuntimeError(f"fetch {url}@{repo['commit']} failed:\n{r.stdout}")
    sh(["git", "checkout", "-q", "FETCH_HEAD"], cwd=dst)
    (dst / ".git" / "info").mkdir(exist_ok=True)
    (dst / ".git" / "info" / "exclude").write_text(".venv/\nnode_modules/\n.sinter/\n.claude/\nAGENTS.md\ntarget/\n")
    if repo.get("setup"):
        r = sh(repo["setup"], cwd=dst, timeout=1800)
        if r.returncode:
            raise RuntimeError(f"setup failed for {dst}:\n{r.stdout[-2000:]}")


def sinter_dir():
    p = shutil.which("sinter")
    return str(Path(p).resolve().parent) if p else None


def arm_env(arm, clone_dir):
    env = dict(os.environ)
    sd = sinter_dir()
    if arm == "baseline":
        if sd:
            env["PATH"] = os.pathsep.join(
                d for d in env["PATH"].split(os.pathsep) if d and str(Path(d).resolve()) != sd)
        return env
    if not sd:
        raise RuntimeError("sinter not on PATH; required for non-baseline arms")
    # Skill card for claude (.claude/skills) and codex (AGENTS.md); graph prebuilt.
    skill = clone_dir / ".claude" / "skills" / "sinter"
    skill.mkdir(parents=True, exist_ok=True)
    shutil.copy(SKILL_CARD, skill / "SKILL.md")
    shutil.copy(SKILL_CARD, clone_dir / "AGENTS.md")
    r = sh(["sinter", "build"], cwd=clone_dir, timeout=900)
    if r.returncode:
        raise RuntimeError(f"sinter build failed:\n{r.stdout[-2000:]}")
    return env


def context_verb_available():
    if not shutil.which("sinter"):
        return False
    r = sh(["sinter", "context", "--help"], cwd=None)
    return r.returncode == 0


# ---------------------------------------------------------------- transcript
def parse_transcript(text):
    """Return (tool_calls, results): tool_calls = [(name, command_or_path)],
    results = {tool_use_id: bytes}. Understands claude/codex JSONL; falls back
    to regex over raw text."""
    calls, results, is_json = [], {}, False
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        is_json = True
        for blk in _blocks(ev):
            t = blk.get("type")
            if t == "tool_use":
                inp = blk.get("input") or {}
                cmd = inp.get("command") or inp.get("cmd") or inp.get("file_path") or inp.get("path") or ""
                if isinstance(cmd, list):
                    cmd = " ".join(map(str, cmd))
                calls.append({"id": blk.get("id"), "name": blk.get("name", ""), "cmd": str(cmd)})
            elif t == "tool_result":
                c = blk.get("content")
                if isinstance(c, list):
                    c = "".join(x.get("text", "") for x in c if isinstance(x, dict))
                results[blk.get("tool_use_id")] = len((c or "").encode())
            elif t in ("function_call", "exec_command_call"):  # codex --json shapes
                cmd = blk.get("command") or blk.get("arguments") or ""
                if isinstance(cmd, list):
                    cmd = " ".join(map(str, cmd))
                calls.append({"id": blk.get("call_id") or blk.get("id"), "name": "shell", "cmd": str(cmd)})
            elif t in ("function_call_output", "exec_command_output"):
                results[blk.get("call_id") or blk.get("id")] = len(str(blk.get("output", "")).encode())
    if not is_json:
        for i, line in enumerate(text.splitlines()):
            if DISCOVERY.search(line):
                calls.append({"id": None, "name": "shell", "cmd": line.strip()})
    return calls, results


def _blocks(ev):
    """Yield content blocks from claude stream-json / codex json events."""
    if isinstance(ev.get("content"), list):
        yield from ev["content"]
    msg = ev.get("message")
    if isinstance(msg, dict) and isinstance(msg.get("content"), list):
        yield from msg["content"]
    item = ev.get("item")
    if isinstance(item, dict):
        yield item
    if ev.get("type") in ("tool_use", "tool_result", "function_call", "function_call_output"):
        yield ev


def classify(call):
    """'sinter', 'discovery', or None (edit/other)."""
    if call["name"] in ("Read", "Glob", "Grep"):
        return "discovery"
    m = DISCOVERY.search(" " + call["cmd"])
    if not m:
        return None
    return "sinter" if m.group(1) == "sinter" else "discovery"


def metrics_from_transcript(text):
    calls, results = parse_transcript(text)
    m = {"discovery_calls": 0, "sinter_calls": 0, "source_bytes_read": 0,
         "sinter_response_bytes": 0, "sinter_max_response_bytes": 0, "fallback_searches_after_sinter": 0}
    seen_sinter = False
    for c in calls:
        kind = classify(c)
        if kind is None:
            continue
        size = results.get(c["id"], 0)
        if kind == "sinter":
            m["sinter_calls"] += 1
            m["sinter_response_bytes"] += size
            m["sinter_max_response_bytes"] = max(m["sinter_max_response_bytes"], size)
            seen_sinter = True
        else:
            m["discovery_calls"] += 1
            m["source_bytes_read"] += size
            if seen_sinter:
                m["fallback_searches_after_sinter"] += 1
    return m


# ---------------------------------------------------------------- one run
def run_one(task, repo, arm, agent, run_idx, out_dir, keep):
    work = Path(tempfile.mkdtemp(prefix=f"agent-eval-{task['id']}-{arm}-"))
    clone_dir = work / "repo"
    row = {"task": task["id"], "repo": task["repo"], "arm": arm, "run": run_idx, "agent": agent["name"],
           "skipped": False, "success": False, "edited_files": [], "wrong_file_edits": [],
           "forbidden_edits": [], "elapsed_s": None, "validate_exit": None, "error": None}
    try:
        clone(repo, clone_dir)
        env = arm_env(arm, clone_dir)
        env.update({
            "SINTER_EVAL_TASK": task["id"], "SINTER_EVAL_ARM": arm,
            "SINTER_EVAL_EXPECTED_FILE": task["expected_files"][0],
            "SINTER_EVAL_PROMPT": prompt_for(task, arm),
        })
        cmd = [a.replace("{prompt}", prompt_for(task, arm)).replace("{harness}", str(HERE)) for a in agent["cmd"]]
        t0 = time.monotonic()
        r = sh(cmd, cwd=clone_dir, env=env, timeout=agent.get("timeout_s", 1800))
        row["elapsed_s"] = round(time.monotonic() - t0, 2)
        transcript = r.stdout or ""
        tdir = out_dir / "transcripts"
        tdir.mkdir(parents=True, exist_ok=True)
        (tdir / f"{task['id']}.{arm}.{run_idx}.txt").write_text(transcript)
        row["agent_exit"] = r.returncode
        row.update(metrics_from_transcript(transcript))
        st = sh(["git", "status", "--porcelain", "--untracked-files=all"], cwd=clone_dir).stdout
        edited = sorted({ln[3:].strip().strip('"') for ln in st.splitlines() if ln.strip()})
        row["edited_files"] = edited
        row["wrong_file_edits"] = [f for f in edited if f not in task["expected_files"]]
        row["forbidden_edits"] = [f for f in edited if f in task.get("forbidden_files", [])]
        row["expected_files_touched"] = [f for f in task["expected_files"] if f in edited]
        for h in task["hidden_files"]:
            dst = clone_dir / h
            dst.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(HIDDEN / task["id"] / h, dst)
        v = sh(task["validate"], cwd=clone_dir, env=env, timeout=1800)
        row["validate_exit"] = v.returncode
        (tdir / f"{task['id']}.{arm}.{run_idx}.validate.txt").write_text(v.stdout or "")
        row["success"] = v.returncode == 0 and not row["forbidden_edits"]
    except Exception as e:  # noqa: BLE001 - record, keep going
        row["error"] = str(e)[:2000]
    finally:
        if keep:
            row["workdir"] = str(work)
        else:
            shutil.rmtree(work, ignore_errors=True)
    return row


def prompt_for(task, arm):
    p = task["request"]
    if arm == "sinter-context":
        p += "\n\nBefore editing, you may run `sinter context \"<your question>\"` for a ranked, evidence-backed map of the relevant code."
    return p + "\n\nMake the change in this repository. Do not commit."


# ---------------------------------------------------------------- scorecard
def median(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


def scorecard(rows):
    by_arm = {a: [r for r in rows if r["arm"] == a and not r["skipped"] and not r["error"]] for a in ARMS}
    summary = {}
    for a, rs in by_arm.items():
        if not rs:
            summary[a] = None
            continue
        summary[a] = {
            "runs": len(rs),
            "success_rate": sum(r["success"] for r in rs) / len(rs),
            "wrong_file_edit_rate": sum(bool(r["wrong_file_edits"]) for r in rs) / len(rs),
            "median_elapsed_s": median([r["elapsed_s"] for r in rs]),
            "median_discovery_calls": median([r.get("discovery_calls") for r in rs]),
            "median_source_bytes_read": median([r.get("source_bytes_read") for r in rs]),
            "median_sinter_calls": median([r.get("sinter_calls") for r in rs]),
            "median_fallback_after_sinter": median([r.get("fallback_searches_after_sinter") for r in rs]),
            "max_sinter_response_bytes": max(r.get("sinter_max_response_bytes", 0) for r in rs),
            "sinter_used_rate": sum(r.get("sinter_calls", 0) > 0 for r in rs) / len(rs),
        }
    gates = {}
    b, s = summary.get("baseline"), summary.get("sinter-optional")
    if b and s:
        def drop(k):
            if not b[k]:
                return None
            return 1 - (s[k] / b[k])
        gates = {
            "success_holds_or_improves": s["success_rate"] >= b["success_rate"],
            "discovery_calls_fall_25pct": (drop("median_discovery_calls") or 0) >= 0.25,
            "source_bytes_fall_25pct": (drop("median_source_bytes_read") or 0) >= 0.25,
            "wrong_file_edits_fall": s["wrong_file_edit_rate"] < b["wrong_file_edit_rate"]
            or (b["wrong_file_edit_rate"] == 0 and s["wrong_file_edit_rate"] == 0),
            "every_sinter_response_within_budget": s["max_sinter_response_bytes"] <= SINTER_BUDGET_BYTES,
        }
        gates["all"] = all(gates.values())
    skipped = sorted({r["arm"] for r in rows if r["skipped"]})
    errors = [(r["task"], r["arm"], r["error"]) for r in rows if r["error"]]
    return {"schema": 1, "sinter_budget_bytes": SINTER_BUDGET_BYTES, "arms": summary,
            "gates": gates, "skipped_arms": skipped, "errors": errors}


def scorecard_md(card, rows):
    out = ["# Coding-agent evaluation scorecard", "",
           "Medians over (task, run) per arm. Gates compare `sinter-optional` to `baseline`.", ""]
    keys = ["runs", "success_rate", "wrong_file_edit_rate", "median_elapsed_s", "median_discovery_calls",
            "median_source_bytes_read", "median_sinter_calls", "median_fallback_after_sinter",
            "max_sinter_response_bytes", "sinter_used_rate"]
    out.append("| metric | " + " | ".join(ARMS) + " |")
    out.append("|---|" + "---|" * len(ARMS))
    for k in keys:
        cells = []
        for a in ARMS:
            v = (card["arms"].get(a) or {}).get(k)
            cells.append("skipped" if card["arms"].get(a) is None else (f"{v:.3g}" if isinstance(v, float) else str(v)))
        out.append(f"| {k} | " + " | ".join(cells) + " |")
    out += ["", "## Adoption gates", ""]
    if card["gates"]:
        for k, v in card["gates"].items():
            out.append(f"- {k}: {'PASS' if v else 'FAIL'}")
    else:
        out.append("- not computable: need both `baseline` and `sinter-optional` rows")
    if card["skipped_arms"]:
        out += ["", f"Skipped arms: {', '.join(card['skipped_arms'])}"]
    if card["errors"]:
        out += ["", "## Harness errors", ""] + [f"- {t} / {a}: {e}" for t, a, e in card["errors"]]
    out += ["", "## Rows", "", "| task | arm | run | success | wrong-file edits | discovery | bytes read | sinter calls | fallback | elapsed s |", "|---|---|---|---|---|---|---|---|---|---|"]
    for r in rows:
        if r["skipped"]:
            continue
        out.append(f"| {r['task']} | {r['arm']} | {r['run']} | {r['success']} | {len(r['wrong_file_edits'])} | "
                   f"{r.get('discovery_calls')} | {r.get('source_bytes_read')} | {r.get('sinter_calls')} | "
                   f"{r.get('fallback_searches_after_sinter')} | {r['elapsed_s']} |")
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--agent", default="claude", help="key in agents.json (claude, codex, fake)")
    ap.add_argument("--agents-config", default=str(AGENTS))
    ap.add_argument("--arms", default=",".join(ARMS))
    ap.add_argument("--tasks", default="", help="comma-separated task ids (default all)")
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--out", default=str(WORKSPACE / "target" / "sinter-agent-eval"))
    ap.add_argument("--keep", action="store_true", help="keep clone directories")
    ap.add_argument("--dry-run", action="store_true",
                    help="fake agent (sh -c) on one task in the sinter-optional arm; proves parse/score/report")
    args = ap.parse_args()

    spec = json.loads(TASKS.read_text())
    agents = json.loads(Path(args.agents_config).read_text())
    tasks = spec["tasks"]
    arms = args.arms.split(",")
    if args.dry_run:
        args.agent, arms, args.runs = "fake", ["sinter-optional"], 1
        tasks = [t for t in tasks if t["id"] == (args.tasks or "sinter-rel-display-curdir")]
    elif args.tasks:
        want = set(args.tasks.split(","))
        tasks = [t for t in tasks if t["id"] in want]
    agent = dict(agents[args.agent], name=args.agent)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    jsonl = out / "results.jsonl"

    ctx_ok = context_verb_available()
    rows = []
    with jsonl.open("w") as fh:
        for task in tasks:
            repo = spec["repositories"][task["repo"]]
            for arm in arms:
                for i in range(args.runs):
                    if arm == "sinter-context" and not ctx_ok:
                        row = {"task": task["id"], "repo": task["repo"], "arm": arm, "run": i, "agent": args.agent,
                               "skipped": True, "error": None, "reason": "`sinter context` not available on PATH"}
                    else:
                        print(f"agent-eval: {task['id']} / {arm} / run {i}", file=sys.stderr)
                        row = run_one(task, repo, arm, agent, i, out, args.keep)
                    rows.append(row)
                    fh.write(json.dumps(row) + "\n")
                    fh.flush()
    card = scorecard(rows)
    (out / "scorecard.json").write_text(json.dumps(card, indent=2))
    (out / "scorecard.md").write_text(scorecard_md(card, rows))
    print((out / "scorecard.md").read_text())
    print(f"wrote {jsonl}, {out / 'scorecard.json'}, {out / 'scorecard.md'}", file=sys.stderr)


if __name__ == "__main__":
    main()
