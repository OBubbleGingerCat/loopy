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

validate_strict_case() {
  local workspace="$1" plan_name="$2" log_file="$3" last_message="$4"
  local db_path="$workspace/.loopy/loopy.db"

  [[ -f "$db_path" ]] || {
    echo "strict validation requires runtime DB: $db_path" >&2
    return 1
  }

  validate_no_mock_gate_artifacts "$workspace" "$last_message"
  validate_no_direct_db_mutation_attempts "$log_file"

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
- Desired plan name: \`$plan_name\`
- Desired task type: \`$task_type\`
- Desired input path: \`draft.md\`
- Use the installed skill entrypoint.
- If runtime helpers are needed, use the installed \`bin/loopy-gen-plan\` helper subcommands directly rather than hunting for a \`loopy:gen-plan\` executable.
- When registering child node ids with \`ensure-node-id\`, always pass \`--parent-relative-path\` pointing at the parent node's self-description markdown path.
- Do not omit \`--parent-relative-path\` for child nodes.
- Do not run leaf review on non-leaf parent nodes.
- Use frontier review for parent nodes that already have child nodes.
- Never mutate \`.loopy/loopy.db\` directly.
- Do not use \`sqlite3\`, Python sqlite writes, or any \`update\`, \`insert\`, \`delete\`, \`alter\`, \`drop\`, or \`create\` statement to repair runtime state.
- If runtime metadata is inconsistent, fail rather than patching the DB.
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

if [[ "$RAN_CASE_COUNT" -eq 0 ]]; then
  echo "no smoke cases executed" >&2
  exit 1
fi

echo "RESULT_SOURCE=direct" >&2
