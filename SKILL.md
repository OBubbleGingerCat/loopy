---
name: "loopy:submit-loop"
description: Use when you need a controlled worker and reviewer loop that returns exactly one terminal success-or-failure result object.
---

# loopy:submit-loop

Use this skill when you need a controlled worker and reviewer loop that produces either one success result or one failure result.

## Input

Collect exactly one logical request object:

```json
{
  "summary": "short task summary",
  "task_type": "coding-task",
  "context": "free-form task context",
  "bypass_sandbox": false,
  "planning_worker": "codex_planner",
  "artifact_worker": "codex_implementer",
  "checkpoint_reviewers": ["codex_scope", "codex_plan", "codex_contract"],
  "artifact_reviewers": ["codex_checkpoint_contract", "codex_correctness", "codex_code_quality"],
  "constraints": {}
}
```

- `summary` is required.
- `task_type` is required.
- `context` is optional and defaults to `""`.
- `bypass_sandbox` is optional and defaults to `false`.
  When `true`, nested worker/reviewer execution uses the bypass executor variant and inherits the caller environment.
- `planning_worker` is optional and defaults from `roles/<task_type>/task-type.toml`.
- `artifact_worker` is optional and defaults from `roles/<task_type>/task-type.toml`.
- `checkpoint_reviewers` is optional and defaults from `roles/<task_type>/task-type.toml`.
- `artifact_reviewers` is optional and defaults from `roles/<task_type>/task-type.toml`.
- `constraints` is optional and defaults to `{}`.
- Reserved caller-visible constraint keys:
  `constraints.smoke_mode = "worker_blocked"` is reserved for the installed coordinator's worker-blocked smoke path. Use it only when you want the coordinator to treat repeated planning-worker runs with `accepted_terminal_api = null` as a protocol-error smoke check and terminate through `finalize-failure`.
- Do not ask the caller for reviewer counts, executor bindings, role file paths, or worktree policy.

## Execution

Hard invariants:

- Treat steps 4, 5, and 6 as one contiguous handoff sequence.
- Before step 4, do only the minimum local work needed to construct the caller request object, resolve the installed skill root, and load the exact installed `coordinator.md` text for handoff.
- Do not spend time on exploratory preflight such as `--help` probes, role-file browsing, repo-state inspection, or extra validation once the required request object is already available.
- Reading `coordinator.md` is for exact prompt handoff only. Do not rewrite it, summarize it, or pause to analyze it before launching the coordinator.
- Once `open-loop` succeeds, the immediate next non-error action must be `spawn_agent`. Do not insert extra shell commands, file reads, repo inspection, commentary-only status updates, or local reasoning detours between parsing the `open-loop` JSON and launching the coordinator.
- If any non-failure action occurs after `open-loop` succeeds but before the coordinator is launched, treat that as a skill violation.

1. Resolve the installed skill root first.
   - For Codex, prefer `$CODEX_HOME/skills/loopy-submit-loop`; otherwise use `$HOME/.codex/skills/loopy-submit-loop`.
   - For Claude Code, use `~/.claude/skills/loopy-submit-loop`.
   - The resolved directory must contain `SKILL.md`, `coordinator.md`, and `bin/loopy`.
2. Use the bundled runtime tool at `${SKILL_ROOT}/bin/loopy`. Do not rely on `loopy` from `PATH`.
3. Read the installed coordinator prompt from `coordinator.md`. Do not replace it with an ad hoc coordinator plan. Do not perform any additional local analysis after loading it.
4. Open the loop first with the bundled runtime from the workspace root using this exact CLI shape:

   `"$SKILL_ROOT/bin/loopy" open-loop --summary <summary> --task-type <task_type>`

   Add optional flags only when the request object includes the field:

   `--context <context>`

   `--planning-worker <planning_worker>`

   `--artifact-worker <artifact_worker>`

   `--checkpoint-reviewers-json <json_array>`

   `--artifact-reviewers-json <json_array>`

   `--constraints-json <json_object>`

   `--bypass-sandbox`
5. Parse `loop_id`, `branch`, and `label` from the `open-loop` JSON and treat them as authoritative loop metadata for the rest of the run. Do not perform any other action except immediate handoff into step 6 unless step 4 failed.
6. Launch a dedicated coordinator subagent from the workspace root with `spawn_agent`. This must be the next non-error action after step 5.
   - Do not fork the caller thread history into the coordinator subagent.
   - Do not override the model for that subagent; let it inherit the caller's current model.
   - Pass only the exact installed `coordinator.md` prompt together with the resolved installed skill root, the pre-opened `loop_id`/`branch`/`label`, and the single caller request object.
   - Do not add extra repo summaries, copied transcript context, failure analysis, or other ad hoc framing around that launch payload.
7. Wait for that dedicated coordinator subagent to finish and capture its final message.

   If the caller needs to inspect progress while waiting for the coordinator, use:

   `"$SKILL_ROOT/bin/loopy" show-loop --loop-id <loop_id> --workspace <workspace_root>`

   Use the original workspace root from the `open-loop` step when polling from another cwd. Add `--json` for machine-readable polling. While waiting, the caller should periodically use this read-only query as a health check to confirm the loop is still progressing or paused in an expected state. This is read-only status inspection; it does not transfer coordinator ownership back to the caller, so use it instead of blind-waiting or resuming coordinator duties locally. Repeated artifact rounds, repeated accepted commits, or a single slow polling interval alone are not evidence of coordinator failure while the coordinator subagent is still running.
   For caller-side polling decisions, treat these `show-loop --json` fields as the supported contract: `status`, `phase`, `plan.latest_submitted_plan_revision`, `plan.current_executable_plan_revision`, `latest_invocation`, `latest_review`, `result`, and `caller_finalize`.
   Worker and reviewer invocations may call `request-timeout-extension` while they run, but that request is advisory only. It does not take effect immediately, it does not consume the terminal submission token, and the runtime-owned retry policy may honor it only after a timeout when the latest request includes concrete progress evidence and a proportionate timeout increase. Each invocation still remains capped at five total attempts.

8. The coordinator owns worktree preparation and worker/reviewer orchestration only. It does not integrate commits or finalize success for the caller branch.
9. If the coordinator returns a caller-finalize handoff object, immediately claim caller-owned finalize with:

   `"$SKILL_ROOT/bin/loopy" begin-caller-finalize --loop-id <loop_id>`

   Then integrate the accepted loop output onto the current caller branch from the workspace root using branch-preserving git operations such as replay or `cherry-pick`. Do not assume `ff-only`.
10. If caller-owned integration succeeds, finalize the loop from the caller with:

   `"$SKILL_ROOT/bin/loopy" finalize-success --loop-id <loop_id> --integration-summary-json <json_object>`

   The `integration_summary_json` request shape is a JSON object using only `strategy`, `landed_commit_shas`, and `resolution_notes`. It must describe the caller-owned replay, including the strategy, the landed caller-branch commit SHAs, and any resolution notes needed to explain conflict handling. Minimal valid example:

   ```json
   {
     "strategy": "cherry_pick",
     "landed_commit_shas": ["abc123"],
     "resolution_notes": null
   }
   ```
11. If caller-owned integration conflicts, attempt task-goal-directed automatic resolution first. If you still cannot safely continue, record the blocked handoff and stop to ask the human with:

   `"$SKILL_ROOT/bin/loopy" block-caller-finalize --loop-id <loop_id> --strategy-summary <summary> --blocking-summary <summary> --human-question <question> --conflicting-files-json <json_array> [--notes <notes>] [--has-in-progress-integration]`

   The `conflicting_files_json` request shape is a JSON array of strings. Minimal valid example:

   ```json
   ["src/foo.rs", "Cargo.toml"]
   ```

   When the human later provides guidance for that same `loop_id`, resume by calling:

   `"$SKILL_ROOT/bin/loopy" begin-caller-finalize --loop-id <loop_id>`

   Continue the integration from the current caller branch and finish through `finalize-success`.
12. If the coordinator subagent errors, returns a non-zero exit status, or does not yield one terminal JSON object, do not continue locally as coordinator. Instead call:

   `"$SKILL_ROOT/bin/loopy" finalize-failure --loop-id <loop_id> --failure-cause-type coordinator_failure --summary <summary>`

   Return only that failure object.
13. Return only the terminal JSON object from `finalize-success` or `finalize-failure`.

## Output

Return exactly one terminal object.

Success shape:

When post-integration cleanup cannot fully remove the disposable private worktree, the success result may also include `cleanup_warnings`. Success results also include caller-facing `improvement_opportunities`, aggregated by latest source with per-source deduplication.

```json
{
  "loop_id": "loop-...",
  "status": "success",
  "artifact_summary": [],
  "commit_summary": [],
  "improvement_opportunities": [],
  "integration_summary": {
    "caller_branch": "main",
    "final_head_sha": "abc123",
    "strategy": "cherry_pick",
    "landed_commit_shas": ["abc123"],
    "resolution_notes": null
  },
  "result_generated_at": "2026-04-09T00:00:00Z",
  "cleanup_warnings": []
}
```

Failure shape:

```json
{
  "loop_id": "loop-...",
  "status": "failure",
  "failure_cause_type": "worker_blocked",
  "summary": "normalized failure summary",
  "source_event_id": 1,
  "phase_at_failure": "planning",
  "last_stable_context": {},
  "worktree_ref": {
    "path": ".loopy/worktrees/submit-12345678",
    "branch": "loopy/loop-...",
    "label": "submit-12345678"
  },
  "result_generated_at": "2026-04-09T00:00:00Z"
}
```

Do not return internal transcript segments, invocation tokens, review-slot state, or a second schema.
