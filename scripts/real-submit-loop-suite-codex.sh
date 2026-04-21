#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ARTIFACT_BASE="${LOOPY_SUITE_ARTIFACT_BASE:-$REPO_ROOT/.humanize/real-submit-loop-suite}"
SOURCE_CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
CASE_TIMEOUT_SEC="${LOOPY_SUITE_CASE_TIMEOUT_SEC:-1800}"
CODEX_ENV_ROOT="$(mktemp -d "$REPO_ROOT/.codex-suite-home.XXXXXX")"
CODEX_HOME_DIR="$CODEX_ENV_ROOT/.codex"
CODEX_SKILL_ROOT="$CODEX_HOME_DIR/skills/loopy-submit-loop"
INSTALL_ROOT="$CODEX_SKILL_ROOT"

mkdir -p "$ARTIFACT_BASE"
RUN_ROOT="${LOOPY_SUITE_RUN_ROOT:-$(mktemp -d "$ARTIFACT_BASE/run.XXXXXX")}"
mkdir -p "$RUN_ROOT"

cleanup() {
  rm -rf "$CODEX_ENV_ROOT"
  if [[ "${LOOPY_SUITE_CLEANUP_RUN_ROOT:-0}" == "1" ]]; then
    rm -rf "$RUN_ROOT"
  fi
}
trap cleanup EXIT

echo "ARTIFACT_ROOT=$RUN_ROOT" >&2

for required in \
  "$SOURCE_CODEX_HOME/config.toml" \
  "$SOURCE_CODEX_HOME/auth.json"; do
  [[ -f "$required" ]] || {
    echo "missing required Codex bootstrap file: $required" >&2
    exit 1
  }
done

mkdir -p "$CODEX_HOME_DIR/skills"
cp "$SOURCE_CODEX_HOME/config.toml" "$CODEX_HOME_DIR/config.toml"
cp "$SOURCE_CODEX_HOME/auth.json" "$CODEX_HOME_DIR/auth.json"
chmod 600 "$CODEX_HOME_DIR/auth.json"

CODEX_HOME="$CODEX_HOME_DIR" \
  "$REPO_ROOT/scripts/install-submit-loop-skill.sh" --target codex >/dev/null

for required in \
  "$INSTALL_ROOT/SKILL.md" \
  "$INSTALL_ROOT/coordinator.md" \
  "$INSTALL_ROOT/roles/coding-task/task-type.toml" \
  "$INSTALL_ROOT/roles/coding-task/planning_worker/codex_planner.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_worker/codex_implementer.md" \
  "$INSTALL_ROOT/roles/coding-task/checkpoint_reviewer/codex_scope.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_reviewer/codex_checkpoint_contract.md" \
  "$INSTALL_ROOT/bin/loopy-submit-loop"; do
  [[ -f "$required" ]] || {
    echo "missing required installed asset: $required" >&2
    exit 1
  }
done

init_workspace() {
  local workspace="$1"
  mkdir -p "$workspace"
  git -C "$workspace" init --initial-branch=main >/dev/null
  git -C "$workspace" config user.name Codex
  git -C "$workspace" config user.email codex@example.com
  printf 'seed\n' >"$workspace/README.md"
  git -C "$workspace" add README.md
  git -C "$workspace" commit -m seed >/dev/null
}

write_prompt() {
  local prompt_file="$1"
  local workspace="$2"
  local request_json="$3"
  cat >"$prompt_file" <<EOF
\$loopy:submit-loop

Use the \`loopy:submit-loop\` skill for the caller request object below.
- Keep your working directory at the workspace root: \`$workspace\`.
- The installed skill is available at \`$CODEX_SKILL_ROOT\`, which resolves to the installed bundle at \`$INSTALL_ROOT\`.
- Follow the installed \`SKILL.md\` contract and let it hand off to the installed \`coordinator.md\`.
- Do not replace the installed skill or coordinator with an ad hoc coordinator plan.
- Do not use or reference any mock role, mock executor, or mock prompt file.
- Do not inspect or print the bundled ELF binary as text.
- Execute the flow end to end and return only the final terminal JSON object.

## Caller request

\`\`\`json
$request_json
\`\`\`
EOF
}

run_prompt() {
  local workspace="$1"
  local prompt_file="$2"
  local last_message_file="$3"
  local attempt_log="$4"
  set +e
  CODEX_HOME="$CODEX_HOME_DIR" timeout --kill-after=30s "${CASE_TIMEOUT_SEC}s" codex exec \
    --full-auto \
    --add-dir "$CODEX_HOME_DIR" \
    -c sandbox_workspace_write.network_access=true \
    -c model_reasoning_effort=high \
    -C "$workspace" \
    -o "$last_message_file" \
    - <"$prompt_file" 2>&1 | tee "$attempt_log"
  local status=${PIPESTATUS[0]}
  set -e
  return "$status"
}

validate_blocked_smoke() {
  local workspace="$1"
  local last_message_file="$2"
  python3 - <<'PY' "$workspace" "$last_message_file"
import json
import pathlib
import sys

workspace = pathlib.Path(sys.argv[1])
payload = json.loads(pathlib.Path(sys.argv[2]).read_text())
assert payload["status"] == "failure", payload
assert payload["failure_cause_type"] == "worker_blocked", payload
assert payload["phase_at_failure"] == "planning", payload
assert workspace.joinpath("README.md").read_text() == "seed\n"
print(json.dumps(payload))
PY
}

validate_readme_append() {
  local workspace="$1"
  local last_message_file="$2"
  python3 - <<'PY' "$workspace" "$last_message_file"
import json
import pathlib
import sys

workspace = pathlib.Path(sys.argv[1])
payload = json.loads(pathlib.Path(sys.argv[2]).read_text())
assert payload["status"] == "success", payload
assert payload["integration_summary"]["strategy"] in {"cherry_pick", "replay", "manual_resolution"}, payload
assert payload["integration_summary"]["landed_commit_shas"], payload
readme = workspace.joinpath("README.md").read_text()
assert "submit-loop real-codex case readme-append" in readme, readme
print(json.dumps(payload))
PY
}

validate_proof_file() {
  local workspace="$1"
  local last_message_file="$2"
  python3 - <<'PY' "$workspace" "$last_message_file"
import json
import pathlib
import sys

workspace = pathlib.Path(sys.argv[1])
payload = json.loads(pathlib.Path(sys.argv[2]).read_text())
assert payload["status"] == "success", payload
proof = workspace.joinpath("docs/proof.txt")
assert proof.read_text() == "real-codex case proof-file\n"
readme = workspace.joinpath("README.md").read_text()
assert "docs/proof.txt" in readme, readme
print(json.dumps(payload))
PY
}

validate_hello_script() {
  local workspace="$1"
  local last_message_file="$2"
  python3 - <<'PY' "$workspace" "$last_message_file"
import json
import pathlib
import stat
import subprocess
import sys

workspace = pathlib.Path(sys.argv[1])
payload = json.loads(pathlib.Path(sys.argv[2]).read_text())
assert payload["status"] == "success", payload
script = workspace.joinpath("bin/hello.sh")
assert script.exists(), script
mode = script.stat().st_mode
assert mode & stat.S_IXUSR, oct(mode)
run = subprocess.run([str(script)], check=True, capture_output=True, text=True)
assert run.stdout == "real-codex suite hello\n", run.stdout
readme = workspace.joinpath("README.md").read_text()
assert "bin/hello.sh" in readme, readme
print(json.dumps(payload))
PY
}

run_case() {
  local case_name="$1"
  local request_json="$2"
  local validator="$3"
  local case_root="$RUN_ROOT/$case_name"
  local workspace="$case_root/workspace"
  local prompt_file="$case_root/prompt.md"
  local last_message_file="$case_root/codex-last-message.json"
  local attempt_log="$case_root/direct.combined.log"

  rm -rf "$case_root"
  mkdir -p "$case_root"
  init_workspace "$workspace"
  write_prompt "$prompt_file" "$workspace" "$request_json"
  rm -f "$last_message_file"

  local run_status=0
  if run_prompt "$workspace" "$prompt_file" "$last_message_file" "$attempt_log"; then
    :
  else
    run_status=$?
    if [[ "$run_status" == "124" ]]; then
      echo "case $case_name timed out after ${CASE_TIMEOUT_SEC}s before producing a terminal message" >&2
    else
      echo "case $case_name failed before producing a terminal message" >&2
    fi
    exit 1
  fi

  "$validator" "$workspace" "$last_message_file"
  printf '%s\n' direct >"$case_root/result-source.txt"
  echo "CASE=$case_name RESULT_SOURCE=direct" >&2
}

run_case \
  case-blocked-smoke \
  '{
  "summary": "case-blocked-smoke",
  "task_type": "coding-task",
  "context": "Real Codex blocked smoke path. There is intentionally no repository task to plan or implement here. Return a blocked terminal outcome rather than inventing work."
}' \
  validate_blocked_smoke

run_case \
  case-readme-append \
  '{
  "summary": "case-readme-append",
  "task_type": "coding-task",
  "context": "Real Codex success path. Append exactly one new line `submit-loop real-codex case readme-append` to README.md in the caller workspace repository. Do not edit any other tracked file."
}' \
  validate_readme_append

run_case \
  case-proof-file \
  '{
  "summary": "case-proof-file",
  "task_type": "coding-task",
  "context": "Real Codex success path. Create docs/proof.txt with exactly the content `real-codex case proof-file` followed by a trailing newline. Add one short sentence to README.md that references docs/proof.txt."
}' \
  validate_proof_file

run_case \
  case-hello-script \
  '{
  "summary": "case-hello-script",
  "task_type": "coding-task",
  "context": "Real Codex success path. Create an executable shell script at bin/hello.sh that prints exactly `real-codex suite hello` followed by a newline. Update README.md with one short sentence that references bin/hello.sh and make sure your verification evidence includes running the script."
}' \
  validate_hello_script

echo "SUITE_STATUS=pass" >&2
