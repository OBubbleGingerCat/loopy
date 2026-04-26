#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ARTIFACT_BASE="${LOOPY_SMOKE_ARTIFACT_BASE:-$REPO_ROOT/.humanize/smoke-runs}"
mkdir -p "$ARTIFACT_BASE"
RUN_ROOT="${LOOPY_SMOKE_RUN_ROOT:-$(mktemp -d "$ARTIFACT_BASE/gen-plan.XXXXXX")}"
WORKSPACES_ROOT="$RUN_ROOT/workspaces"
LOG_DIR="$RUN_ROOT/logs"
PROMPT_DIR="$RUN_ROOT/prompts"
LAST_MESSAGE_DIR="$RUN_ROOT/last-messages"
SOURCE_CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
STRICT_VALIDATION="${LOOPY_SMOKE_STRICT_VALIDATION:-1}"
CASE_FILTER="${LOOPY_SMOKE_CASE_FILTER:-}"
KNOWN_CASES=(
  rust-cli-todo
  fastapi-notes-api
  csv-export-rust-report
  refine-api-plan
  refine-malformed-comments
)
RAN_CASE_COUNT=0
CODEX_ENV_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/loopy-gen-plan-codex-home.XXXXXX")"
CODEX_HOME_DIR="$CODEX_ENV_ROOT/.codex"
CODEX_SKILL_ROOT="$CODEX_HOME_DIR/skills/loopy-gen-plan"
INSTALL_ROOT="$CODEX_SKILL_ROOT"

cleanup() {
  rm -rf "$CODEX_ENV_ROOT"
  if [[ "${LOOPY_SMOKE_CLEANUP_RUN_ROOT:-0}" == "1" ]]; then
    rm -rf "$RUN_ROOT"
  fi
}
trap cleanup EXIT

mkdir -p \
  "$WORKSPACES_ROOT" \
  "$LOG_DIR" \
  "$PROMPT_DIR" \
  "$LAST_MESSAGE_DIR" \
  "$CODEX_HOME_DIR/skills"

echo "ARTIFACT_ROOT=$RUN_ROOT" >&2

for required in \
  "$SOURCE_CODEX_HOME/config.toml" \
  "$SOURCE_CODEX_HOME/auth.json"; do
  [[ -f "$required" ]] || {
    echo "missing required Codex bootstrap file: $required" >&2
    exit 1
  }
done

cp "$SOURCE_CODEX_HOME/config.toml" "$CODEX_HOME_DIR/config.toml"
cp "$SOURCE_CODEX_HOME/auth.json" "$CODEX_HOME_DIR/auth.json"
chmod 600 "$CODEX_HOME_DIR/auth.json"

CODEX_HOME="$CODEX_HOME_DIR" \
  "$REPO_ROOT/scripts/install-gen-plan-skill.sh" --target codex >/dev/null

for required in \
  "$INSTALL_ROOT/SKILL.md" \
  "$INSTALL_ROOT/gen-plan.toml" \
  "$INSTALL_ROOT/prompts/domain_contract.md" \
  "$INSTALL_ROOT/prompts/leaf_runtime.md" \
  "$INSTALL_ROOT/prompts/frontier_runtime.md" \
  "$INSTALL_ROOT/prompts/refine_instructions.md" \
  "$INSTALL_ROOT/roles/coding-task/task-type.toml" \
  "$INSTALL_ROOT/roles/coding-task/leaf_reviewer/codex_default.md" \
  "$INSTALL_ROOT/roles/coding-task/frontier_reviewer/codex_default.md" \
  "$INSTALL_ROOT/bin/loopy-gen-plan"; do
  [[ -f "$required" ]] || {
    echo "missing required installed asset: $required" >&2
    exit 1
  }
done

seed_workspace() {
  local workspace="$1"
  git -C "$workspace" init --initial-branch=main >/dev/null
  git -C "$workspace" config user.name Codex
  git -C "$workspace" config user.email codex@example.com
  printf 'seed\n' >"$workspace/README.md"
  git -C "$workspace" add README.md
  git -C "$workspace" commit -m seed >/dev/null
}

make_workspace() {
  local case_name="$1"
  local workspace="$WORKSPACES_ROOT/$case_name"
  mkdir -p "$workspace"
  seed_workspace "$workspace"
  printf '%s\n' "$workspace"
}

run_prompt() {
  local workspace="$1" prompt_file="$2" last_message="$3" log_file="$4" status=0
  set +e
  CODEX_HOME="$CODEX_HOME_DIR" codex exec \
    --full-auto \
    -c sandbox_workspace_write.network_access=true \
    -c model_reasoning_effort=high \
    -C "$workspace" \
    -o "$last_message" \
    - <"$prompt_file" 2>&1 | tee "$log_file"
  status=${PIPESTATUS[0]}
  set -e
  return "$status"
}

validate_plan_tree() {
  local workspace="$1" plan_name="$2"
  local plan_root="$workspace/.loopy/plans/$plan_name"

  [[ -d "$plan_root" ]] || {
    echo "missing plan root: $plan_root" >&2
    return 1
  }
  find "$plan_root" -name '*.md' | grep -q . || {
    echo "expected markdown nodes under $plan_root" >&2
    return 1
  }
}

validate_no_mock_gate_artifacts() {
  local workspace="$1" last_message="$2"
  python3 - "$workspace" "$last_message" <<'PY'
import pathlib
import sys

workspace = pathlib.Path(sys.argv[1])
last_message = pathlib.Path(sys.argv[2])
artifacts = []
if last_message.is_file():
    artifacts.append(last_message)
gate_runs = workspace / ".loopy" / "gate-runs"
if gate_runs.is_dir():
    artifacts.extend(sorted(gate_runs.glob("*/last-message.json")))

needles = [
    '"reviewer_role_id":"mock"',
    '"reviewer_role_id": "mock"',
    "Task 4 uses deterministic mock reviewer execution.",
    "Mock leaf review requires a revision.",
    "Mock frontier review invalidated a leaf.",
    "Mock frontier review found no child leaves to invalidate.",
]

for artifact in artifacts:
    text = artifact.read_text(encoding="utf-8", errors="ignore")
    for needle in needles:
        if needle in text:
            sys.stderr.write(
                f"invalid mock reviewer marker in {artifact}: {needle}\n"
            )
            sys.exit(1)
PY
}

validate_no_direct_db_mutation_attempts() {
  local log_file="$1"
  python3 - "$log_file" <<'PY'
import pathlib
import re
import sys

log_path = pathlib.Path(sys.argv[1])
text = log_path.read_text(encoding="utf-8", errors="ignore")
command_blocks = re.findall(
    r"(?ms)^exec\s*\n(.*?)(?=^\s*(?:succeeded|failed) in )",
    text,
)
patterns = [
    re.compile(
        r"(?is)(?:\bsqlite3?\b|import\s+sqlite3|python3?.{0,80}sqlite3)"
        r".{0,400}\.loopy/loopy\.db.{0,400}\b(update|insert|delete|alter|drop|create|replace|vacuum|reindex)\b"
    ),
    re.compile(
        r"(?is)\.loopy/loopy\.db.{0,400}"
        r"(?:\bsqlite3?\b|import\s+sqlite3|python3?.{0,80}sqlite3)"
        r".{0,400}\b(update|insert|delete|alter|drop|create|replace|vacuum|reindex)\b"
    ),
]

for block in command_blocks:
    for pattern in patterns:
        match = pattern.search(block)
        if match:
            snippet = block[max(0, match.start() - 120):match.end() + 120].strip()
            sys.stderr.write(
                f"detected direct sqlite write attempt against .loopy/loopy.db in {log_path}:\n{snippet}\n"
            )
            sys.exit(1)
PY
}

validate_no_direct_db_read_attempts() {
  local log_file="$1"
  python3 - "$log_file" <<'PY'
import pathlib
import re
import sys

log_path = pathlib.Path(sys.argv[1])
text = log_path.read_text(encoding="utf-8", errors="ignore")
block_pattern = re.compile(
    r"(?ms)^exec\s*\n(?P<command>.+?)\n (?P<status>succeeded|exited \d+|failed)\b.*?:\n(?P<output>.*?)(?=^exec\s*\n|\Z)"
)

command_read_pattern = re.compile(
    r"(?is)\.loopy/loopy\.db.{0,200}\b(cat|sed|head|tail|strings|less|more|hexdump|xxd|file)\b"
)
command_pipe_pattern = re.compile(
    r"(?is)\b(find|rg|fd)\b.{0,300}\.loopy.{0,300}\b(xargs|while|for)\b.{0,300}\b(cat|sed|head|tail|strings|less|more|hexdump|xxd)\b"
)
output_read_pattern = re.compile(
    r"(?is)(---\s+\.loopy/loopy\.db\s+---|SQLite format 3)"
)

for match in block_pattern.finditer(text):
    command = match.group("command")
    output = match.group("output")
    if command_read_pattern.search(command):
        sys.stderr.write(
            f"detected direct sqlite read attempt against .loopy/loopy.db in {log_path}:\n{command.strip()}\n"
        )
        sys.exit(1)
    if command_pipe_pattern.search(command) and output_read_pattern.search(output):
        sys.stderr.write(
            f"detected indirect text inspection of .loopy/loopy.db in {log_path}:\n{command.strip()}\n"
        )
        sys.exit(1)
PY
}

validate_runtime_api_transcript_usage() {
  local log_file="$1"
  python3 - "$log_file" <<'PY'
import pathlib
import re
import sys

log_path = pathlib.Path(sys.argv[1])
text = log_path.read_text(encoding="utf-8", errors="ignore")

helper_pattern = (
    r'(?:'
    r'"\$bin"'
    r'|'
    r'"[^"\n]*/loopy-gen-plan"'
    r'|'
    r"'[^'\n]*/loopy-gen-plan'"
    r'|'
    r"[^'\"\s\n]*/loopy-gen-plan"
    r')'
)

subcommand_pattern = re.compile(
    helper_pattern
    + r"""(?:\s+--[A-Za-z0-9_-]+(?:\s+(?:"[^"\n]*"|'[^'\n]*'|[^\s"'\n]+))?)*"""
    + r'\s+'
    + r'(ensure-plan|open-plan|ensure-node-id|run-leaf-review-gate|run-frontier-review-gate|mock-leaf-reviewer|mock-frontier-reviewer)'
    + r'\b(?!\s+--help)'
)

invocation_pattern = re.compile(
    r'(?ms)^exec\s*\n(?P<command>.+?)(?=^exec\s*$|^\s*(?:succeeded|exited \d+|failed) in\b|\Z)'
)

runtime_records = []
for match in invocation_pattern.finditer(text):
    command = match.group("command")
    subcommand_match = subcommand_pattern.search(command)
    if not subcommand_match:
        continue
    runtime_records.append(
        {
            "api": subcommand_match.group(1),
            "position": match.start(),
            "command": command,
        }
    )

if not runtime_records:
    sys.stderr.write(
        f"strict validation found no actual loopy-gen-plan runtime API calls in {log_path}\n"
    )
    sys.exit(1)

positions = {}
for record in runtime_records:
    positions.setdefault(record["api"], []).append(record["position"])

required = [
    "ensure-plan",
    "open-plan",
    "ensure-node-id",
    "run-leaf-review-gate",
    "run-frontier-review-gate",
]
for api in required:
    if api not in positions:
        sys.stderr.write(
            f"strict validation missing required runtime API `{api}` in {log_path}\n"
        )
        sys.exit(1)

for forbidden in ("mock-leaf-reviewer", "mock-frontier-reviewer"):
    if forbidden in positions:
        sys.stderr.write(
            f"strict validation saw forbidden mock runtime API `{forbidden}` in {log_path}\n"
        )
        sys.exit(1)

ordering = [
    "ensure-plan",
    "open-plan",
    "ensure-node-id",
    "run-leaf-review-gate",
    "run-frontier-review-gate",
]
for earlier, later in zip(ordering, ordering[1:]):
    if positions[earlier][0] >= positions[later][0]:
        sys.stderr.write(
            "strict validation saw runtime APIs out of order in "
            f"{log_path}: expected `{earlier}` before `{later}`\n"
        )
        sys.exit(1)
PY
}

validate_strict_case() {
  local workspace="$1" plan_name="$2" log_file="$3" last_message="$4"
  local db_path="$workspace/.loopy/loopy.db"

  [[ -f "$db_path" ]] || {
    echo "strict validation requires runtime DB: $db_path" >&2
    return 1
  }

  validate_no_mock_gate_artifacts "$workspace" "$last_message"
  validate_no_direct_db_mutation_attempts "$log_file"
  validate_no_direct_db_read_attempts "$log_file"
  validate_runtime_api_transcript_usage "$log_file"

  python3 - "$db_path" "$plan_name" <<'PY'
import sqlite3
import sys

db_path, plan_name = sys.argv[1], sys.argv[2]
connection = sqlite3.connect(db_path)

def scalar(sql, params=()):
    row = connection.execute(sql, params).fetchone()
    return row[0] if row else None

plan_id = scalar(
    "SELECT plan_id FROM GEN_PLAN__plans WHERE plan_name = ?",
    (plan_name,),
)
if not plan_id:
    sys.stderr.write(f"strict validation missing plan row for {plan_name} in {db_path}\n")
    sys.exit(1)

leaf_non_mock = scalar(
    "SELECT COUNT(*) FROM GEN_PLAN__leaf_gate_runs WHERE plan_id = ? AND reviewer_role_id <> 'mock'",
    (plan_id,),
)
frontier_non_mock = scalar(
    "SELECT COUNT(*) FROM GEN_PLAN__frontier_gate_runs WHERE plan_id = ? AND reviewer_role_id <> 'mock'",
    (plan_id,),
)
leaf_mock = scalar(
    "SELECT COUNT(*) FROM GEN_PLAN__leaf_gate_runs WHERE plan_id = ? AND reviewer_role_id = 'mock'",
    (plan_id,),
)
frontier_mock = scalar(
    "SELECT COUNT(*) FROM GEN_PLAN__frontier_gate_runs WHERE plan_id = ? AND reviewer_role_id = 'mock'",
    (plan_id,),
)
non_flat_nodes = scalar(
    "SELECT COUNT(*) FROM GEN_PLAN__nodes WHERE plan_id = ? AND parent_node_id IS NOT NULL",
    (plan_id,),
)

if leaf_non_mock < 1:
    sys.stderr.write(
        f"strict validation expected non-mock leaf gate usage for {plan_name} in {db_path}\n"
    )
    sys.exit(1)
if frontier_non_mock < 1:
    sys.stderr.write(
        f"strict validation expected non-mock frontier gate usage for {plan_name} in {db_path}\n"
    )
    sys.exit(1)
if non_flat_nodes < 1:
    sys.stderr.write(
        f"strict validation expected non-flat node metadata for {plan_name} in {db_path}\n"
    )
    sys.exit(1)
if leaf_mock or frontier_mock:
    sys.stderr.write(
        f"strict validation rejected mock gate rows for {plan_name} in {db_path}\n"
    )
    sys.exit(1)
PY
}

should_run_case() {
  local case_name="$1"
  if [[ -z "$CASE_FILTER" ]]; then
    return 0
  fi

  local candidate
  IFS=',' read -r -a case_names <<<"$CASE_FILTER"
  for candidate in "${case_names[@]}"; do
    if [[ "$candidate" == "$case_name" ]]; then
      return 0
    fi
  done
  return 1
}

validate_case_filter() {
  [[ -z "$CASE_FILTER" ]] && return 0

  local requested known found
  IFS=',' read -r -a requested_cases <<<"$CASE_FILTER"
  for requested in "${requested_cases[@]}"; do
    [[ -n "$requested" ]] || {
      echo "empty smoke case name in LOOPY_SMOKE_CASE_FILTER" >&2
      exit 1
    }

    found=0
    for known in "${KNOWN_CASES[@]}"; do
      if [[ "$requested" == "$known" ]]; then
        found=1
        break
      fi
    done

    if [[ "$found" -ne 1 ]]; then
      echo "unknown smoke case in LOOPY_SMOKE_CASE_FILTER: $requested" >&2
      echo "known cases: ${KNOWN_CASES[*]}" >&2
      exit 1
    fi
  done
}

run_case() {
  local case_name="$1" plan_name="$2" task_type="$3" draft_text="$4"
  local workspace="$WORKSPACES_ROOT/$case_name"
  local prompt_file="$PROMPT_DIR/$case_name.prompt.md"
  local last_message="$LAST_MESSAGE_DIR/$case_name.json"
  local log_file="$LOG_DIR/$case_name.log"

  workspace="$(make_workspace "$case_name")"
  printf '%s\n' "$draft_text" >"$workspace/draft.md"

  if [[ "$case_name" == "csv-export-rust-report" ]]; then
    mkdir -p "$workspace/src"
    cat >"$workspace/Cargo.toml" <<'EOF'
[package]
name = "reporting"
version = "0.1.0"
edition = "2024"
EOF
    cat >"$workspace/src/lib.rs" <<'EOF'
pub fn render_report() -> String {
    "report".to_owned()
}
EOF
    git -C "$workspace" add Cargo.toml src/lib.rs
    git -C "$workspace" commit -m "seed rust crate" >/dev/null
  fi

  cat >"$prompt_file" <<EOF
Skill name: \`loopy:gen-plan\`

Use the \`loopy:gen-plan\` skill.
- Keep your working directory at \`$workspace\`.
- The installed skill is available at \`$CODEX_SKILL_ROOT\`, which resolves to \`$INSTALL_ROOT\`.
- \`loopy:gen-plan\` is the skill name, not a shell command.
- Do not try to execute \`loopy:gen-plan\` from the shell.
- Treat the desired plan name, task type, and input path as semantic inputs rather than a shell command line.
- Treat the installed runtime APIs as the only authoritative source of plan runtime state.
- A plan is not established until installed \`ensure-plan\` or \`open-plan\` succeeds.
- A node is not tracked until installed \`ensure-node-id\` succeeds.
- A review gate has not happened unless installed \`run-leaf-review-gate\` or \`run-frontier-review-gate\` returns a valid gate result.
- Always invoke installed runtime helpers against the project workspace root, not a nested \`.loopy/plans/\` directory.
- Do not self-review, hand-wave, or write free-text reviewer verdicts in place of runtime gate output.
- Do not fabricate plan ids, node ids, reviewer identities, gate summaries, or issue lists.
- Do not inspect \`.loopy/loopy.db\` directly, including broad file-dump commands that would read it as text.
- Desired plan name: \`$plan_name\`
- Desired task type: \`$task_type\`
- Desired input path: \`draft.md\`
- For this smoke, if packaging or crate metadata needs a license decision, use \`MIT\` as an explicitly user-approved default.
- Use the installed skill entrypoint.
- If runtime helpers are needed, use the installed \`bin/loopy-gen-plan\` helper subcommands directly rather than hunting for a \`loopy:gen-plan\` executable.
- Use installed \`ensure-plan\`, then installed \`open-plan\`, before continuing with tracked plan work.
- If installed \`ensure-plan\`, \`open-plan\`, or \`ensure-node-id\` fails because of request construction or missing prerequisite runtime state, use the returned runtime error plus the current plan tree/runtime state to repair the runtime call sequence.
- During runtime-call recovery for \`ensure-plan\`, \`open-plan\`, or \`ensure-node-id\`, do not change plan content.
- Do not blindly guess parameters or keep replaying the same class of runtime error without new runtime evidence or relevant state changes.
- Treat markdown targets as the only canonical node identities for installed \`ensure-node-id\`.
- Canonical node path shapes are: root leaf \`leaf.md\`, root child parent \`scope/scope.md\`, nested leaf \`parent/leaf.md\`, nested parent \`parent/child/child.md\`.
- Never register a directory path as a node target, and never register a node path without \`.md\`.
- When registering child node ids with \`ensure-node-id\`, always pass \`--parent-relative-path\` pointing at the parent node's self-description markdown path.
- Register parent nodes first; do not rely on installed \`ensure-node-id\` to invent missing parent runtime state.
- Treat child registration as direct-child registration under the tracked parent markdown target rather than a recursive descendant shortcut.
- Do not omit \`--parent-relative-path\` for child nodes.
- Do not run leaf review on non-leaf parent nodes.
- Use frontier review for parent nodes that already have child nodes.
- Never mutate \`.loopy/loopy.db\` directly.
- Never read \`.loopy/loopy.db\` directly as a planning aid or recovery shortcut.
- Do not use \`sqlite3\`, Python sqlite writes, or any \`update\`, \`insert\`, \`delete\`, \`alter\`, \`drop\`, or \`create\` statement to repair runtime state.
- If runtime metadata is inconsistent, fail rather than patching the DB.
- If installed \`run-leaf-review-gate\` or \`run-frontier-review-gate\` fails to launch, times out, fails to write the expected runtime artifact, or fails to return parseable valid output, immediately retry the same gate call up to 5 times without changing files, ids, or arguments.
- If all 5 immediate retries fail for the same gate call, stop and surface the combined failure instead of bypassing the gate.
- If a gate call succeeds and returns review issues, revise the plan and then submit a new gate call; do not treat review issues as a retry case.
- Do not inline the installed skill files into the prompt.
- Do not inspect or print the installed \`bin/loopy-gen-plan\` ELF binary as text.
- Do not run \`cat\`, \`sed\`, \`head\`, \`tail\`, \`strings\`, \`less\`, \`more\`, \`hexdump\`, \`xxd\`, or similar text inspection commands against that ELF binary.
- Do not use \`apply_patch\` in this smoke.
- Write the plan artifacts with shell file-writing commands.
- Use \`mkdir -p\`, shell redirection, and \`cat > file\` style commands instead.
- If you need to update any artifact under \`.loopy/plans/$plan_name/\`, rewrite the whole file with shell commands instead of patching it.
- Use auto mode.
- Continue automatically until the plan is ready.
- Require real reviewer behavior only.
- Use the real installed \`coding-task\` reviewer defaults; do not switch to mock reviewers.
- If any installed runtime gate command reports \`reviewer_role_id=mock\`, reject that output as invalid for this smoke.
- If any installed runtime gate command reports the rationale \`Task 4 uses deterministic mock reviewer execution.\`, reject that output as invalid for this smoke.
- If any installed runtime gate command reports the summaries \`Mock leaf review requires a revision.\`, \`Mock frontier review invalidated a leaf.\`, or \`Mock frontier review found no child leaves to invalidate.\`, reject that output as invalid for this smoke.
- When those invalid mock outputs appear, continue with the installed \`codex_default\` reviewer instructions instead of accepting the mock review.
- Keep the generated artifacts under \`.loopy/plans/$plan_name/\`.

$draft_text
EOF

  if ! run_prompt "$workspace" "$prompt_file" "$last_message" "$log_file"; then
    echo "gen-plan smoke case $case_name failed; see $log_file" >&2
    return 1
  fi

  validate_plan_tree "$workspace" "$plan_name"
  if [[ "$STRICT_VALIDATION" != "0" ]]; then
    validate_strict_case "$workspace" "$plan_name" "$log_file" "$last_message"
  fi

  RAN_CASE_COUNT=$((RAN_CASE_COUNT + 1))
}

setup_refine_fixture() {
  local workspace="$1" case_name="$2" plan_name="$3" malformed="$4"
  local plan_root="$workspace/.loopy/plans/$plan_name"
  local state_dir="$RUN_ROOT/fixture-state/$case_name"
  local helper="$INSTALL_ROOT/bin/loopy-gen-plan"
  mkdir -p "$plan_root/api" "$state_dir"

  cat >"$plan_root/$plan_name.md" <<EOF
# $plan_name

## Scope
Refine smoke fixture.

## Child Nodes
- [API](./api/api.md)
EOF

  cat >"$plan_root/api/api.md" <<'EOF'
# API

## Scope
API planning scope.

## Child Nodes
- [Add Auth Tests](./add-auth-tests.md)
EOF

  if [[ "$malformed" == "1" ]]; then
    cat >"$plan_root/api/add-auth-tests.md" <<'EOF'
# Add Auth Tests

## Goal
Add focused authentication regression tests.

BEGIN_COMMENT
Please tighten the acceptance criteria.
BEGIN_COMMENT
Nested marker should fail closed.
END_COMMENT
EOF
  else
    cat >"$plan_root/api/add-auth-tests.md" <<'EOF'
# Add Auth Tests

## Goal
Add focused authentication regression tests.

BEGIN_COMMENT
Please require one negative token case and one successful token case in the acceptance criteria.
END_COMMENT

## Acceptance Criteria
- Existing API test coverage remains documented.
EOF
  fi

  "$helper" --workspace "$workspace" ensure-plan \
    --plan-name "$plan_name" \
    --task-type coding-task \
    --project-directory "$workspace" >"$state_dir/ensure-plan.json"
  local plan_id
  plan_id="$(python3 - "$state_dir/ensure-plan.json" <<'PY'
import json
import sys
print(json.load(open(sys.argv[1]))["plan_id"])
PY
)"
  "$helper" --workspace "$workspace" ensure-node-id \
    --plan-id "$plan_id" \
    --relative-path "$plan_name.md" >"$state_dir/ensure-node-root.json"
  "$helper" --workspace "$workspace" ensure-node-id \
    --plan-id "$plan_id" \
    --relative-path "api/api.md" >"$state_dir/ensure-node-parent.json"
  "$helper" --workspace "$workspace" ensure-node-id \
    --plan-id "$plan_id" \
    --relative-path "api/add-auth-tests.md" \
    --parent-relative-path "api/api.md" >"$state_dir/ensure-node-leaf.json"
}

run_refine_case() {
  local case_name="$1" plan_name="$2" malformed="$3"
  local workspace="$WORKSPACES_ROOT/$case_name"
  local prompt_file="$PROMPT_DIR/$case_name.prompt.md"
  local last_message="$LAST_MESSAGE_DIR/$case_name.json"
  local log_file="$LOG_DIR/$case_name.log"

  workspace="$(make_workspace "$case_name")"
  setup_refine_fixture "$workspace" "$case_name" "$plan_name" "$malformed"

  if [[ "$malformed" == "1" ]]; then
    cat >"$prompt_file" <<EOF
Skill name: \`loopy:gen-plan\`

Use the \`loopy:gen-plan --refine <plan-name>\` skill invocation contract for plan \`$plan_name\`.
- Desired plan name: \`$plan_name\`
- \`loopy:gen-plan --refine <plan-name>\` is a skill invocation contract, not a shell command.
- Do not execute \`loopy:gen-plan\` as a shell command.
- Use installed runtime helper \`open-plan\` before comment discovery.
- Discover literal \`BEGIN_COMMENT\` and \`END_COMMENT\` markers.
- This case intentionally contains malformed nested comment markers.
- Fail closed after malformed comment discovery.
- Do not run \`run-leaf-review-gate\` or \`run-frontier-review-gate\` after malformed comment discovery.
- Do not inspect or mutate \`.loopy/loopy.db\` directly.
EOF
  else
    cat >"$prompt_file" <<EOF
Skill name: \`loopy:gen-plan\`

Use the \`loopy:gen-plan --refine <plan-name>\` skill invocation contract for plan \`$plan_name\`.
- Desired plan name: \`$plan_name\`
- \`loopy:gen-plan --refine <plan-name>\` is a skill invocation contract, not a shell command.
- Do not execute \`loopy:gen-plan\` as a shell command.
- Treat \`BEGIN_COMMENT\` and \`END_COMMENT\` blocks as natural-language feedback.
- Use installed \`open-plan\` before comment discovery.
- Use installed \`inspect-node\` or \`list-children\` for tracked runtime state.
- Use installed \`ensure-node-id\` for any new refined nodes.
- Run installed \`run-leaf-review-gate\` before any installed \`run-frontier-review-gate\`.
- Do not inspect or mutate \`.loopy/loopy.db\` directly.
- Refine the existing tracked plan in place.
EOF
  fi

  if ! run_prompt "$workspace" "$prompt_file" "$last_message" "$log_file"; then
    echo "gen-plan refine smoke case $case_name failed; see $log_file" >&2
    return 1
  fi

  validate_plan_tree "$workspace" "$plan_name"
  if [[ "$STRICT_VALIDATION" != "0" && "$malformed" != "1" ]]; then
    validate_strict_case "$workspace" "$plan_name" "$log_file" "$last_message"
  fi

  RAN_CASE_COUNT=$((RAN_CASE_COUNT + 1))
}

validate_case_filter

if should_run_case rust-cli-todo; then
  run_case \
    rust-cli-todo \
    rust-cli-todo \
    coding-task \
    'Create a plan for a tiny Rust CLI todo app using clap and a JSON file store. Include add/list/done flows, tests, and packaging.'
fi

if should_run_case fastapi-notes-api; then
  run_case \
    fastapi-notes-api \
    fastapi-notes-api \
    coding-task \
    'Create a plan for a tiny FastAPI notes API with create/list/delete endpoints, pydantic models, pytest coverage, and local sqlite development.'
fi

if should_run_case csv-export-rust-report; then
  run_case \
    csv-export-rust-report \
    csv-export-rust-report \
    coding-task \
    'Create a plan for adding CSV export support to the existing Rust reporting crate. Assume the raw report input contract is `&str` with newline-delimited rows and comma-delimited fields. Include parser changes, reporting APIs, regression tests, and documentation.'
fi

if should_run_case refine-api-plan; then
  run_refine_case refine-api-plan refine-api-plan 0
fi

if should_run_case refine-malformed-comments; then
  run_refine_case refine-malformed-comments refine-malformed-comments 1
fi

if [[ "$RAN_CASE_COUNT" -eq 0 ]]; then
  echo "no smoke cases executed" >&2
  exit 1
fi

echo "RESULT_SOURCE=direct" >&2
