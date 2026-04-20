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

run_case() {
  local case_name="$1" plan_name="$2" invocation="$3" draft_text="$4"
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
\$loopy:gen-plan

Use the \`loopy:gen-plan\` skill.
- Keep your working directory at \`$workspace\`.
- The installed skill is available at \`$CODEX_SKILL_ROOT\`, which resolves to \`$INSTALL_ROOT\`.
- Treat the skill name \`loopy:gen-plan\` as the installed entrypoint.
- Use the installed skill entrypoint.
- Do not hunt for a shell alias, alternate binary, or wrapper for \`loopy:gen-plan\`.
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
- Create the plan with \`$ $invocation\`.
- Keep the generated artifacts under \`.loopy/plans/$plan_name/\`.

$draft_text
EOF

  if ! run_prompt "$workspace" "$prompt_file" "$last_message" "$log_file"; then
    echo "gen-plan smoke case $case_name failed; see $log_file" >&2
    return 1
  fi

  validate_plan_tree "$workspace" "$plan_name"
}

run_case \
  rust-cli-todo \
  rust-cli-todo \
  'loopy:gen-plan --input draft.md --plan-name rust-cli-todo --task-type coding-task' \
  'Create a plan for a tiny Rust CLI todo app using clap and a JSON file store. Include add/list/done flows, tests, and packaging.'

run_case \
  fastapi-notes-api \
  fastapi-notes-api \
  'loopy:gen-plan --input draft.md --plan-name fastapi-notes-api --task-type coding-task' \
  'Create a plan for a tiny FastAPI notes API with create/list/delete endpoints, pydantic models, pytest coverage, and local sqlite development.'

run_case \
  csv-export-rust-report \
  csv-export-rust-report \
  'loopy:gen-plan --input draft.md --plan-name csv-export-rust-report --task-type coding-task' \
  'Create a plan for adding CSV export support to the existing Rust reporting crate, including parser changes, reporting APIs, regression tests, and documentation.'

echo "RESULT_SOURCE=direct" >&2
