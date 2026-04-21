#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ARTIFACT_BASE="${LOOPY_SMOKE_ARTIFACT_BASE:-$REPO_ROOT/.humanize/smoke-runs}"
mkdir -p "$ARTIFACT_BASE"
RUN_ROOT="${LOOPY_SMOKE_RUN_ROOT:-$(mktemp -d "$ARTIFACT_BASE/run.XXXXXX")}"
mkdir -p "$RUN_ROOT"
WORKSPACE="$RUN_ROOT/workspace"
LOG_DIR="$RUN_ROOT/logs"
SOURCE_CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
ALLOW_TRANSPORT_FALLBACK="${LOOPY_SMOKE_ALLOW_TRANSPORT_FALLBACK:-0}"
ATTEMPT_TIMEOUT_SEC="${LOOPY_SMOKE_ATTEMPT_TIMEOUT_SEC:-900}"
CODEX_ENV_ROOT="$(mktemp -d "$REPO_ROOT/.codex-smoke-home.XXXXXX")"
CODEX_HOME_DIR="$CODEX_ENV_ROOT/.codex"
CODEX_SKILL_ROOT="$CODEX_HOME_DIR/skills/loopy-submit-loop"
INSTALL_ROOT="$CODEX_SKILL_ROOT"
PROMPT_FILE="$RUN_ROOT/codex-smoke-prompt.md"
LAST_MESSAGE_FILE="$RUN_ROOT/codex-last-message.json"

cleanup() {
  rm -rf "$CODEX_ENV_ROOT"
  if [[ "${LOOPY_SMOKE_CLEANUP_RUN_ROOT:-0}" == "1" ]]; then
    rm -rf "$RUN_ROOT"
  fi
}
trap cleanup EXIT

mkdir -p "$WORKSPACE" "$LOG_DIR"
echo "ARTIFACT_ROOT=$RUN_ROOT" >&2

git -C "$WORKSPACE" init --initial-branch=main >/dev/null
git -C "$WORKSPACE" config user.name Codex
git -C "$WORKSPACE" config user.email codex@example.com
printf 'seed\n' >"$WORKSPACE/README.md"
git -C "$WORKSPACE" add README.md
git -C "$WORKSPACE" commit -m seed >/dev/null

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

cd "$WORKSPACE"

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

cat >"$PROMPT_FILE" <<EOF
\$loopy:submit-loop

Use the \`loopy:submit-loop\` skill for the caller request object below.
- Keep your working directory at the workspace root: \`$WORKSPACE\`.
- The installed skill is available at \`$CODEX_SKILL_ROOT\`, which resolves to the installed bundle at \`$INSTALL_ROOT\`.
- Follow the installed \`SKILL.md\` contract and let it hand off to the installed \`coordinator.md\`.
- Do not replace the installed skill or coordinator with an ad hoc coordinator plan.
- Do not inspect or print the bundled ELF binary as text.
- Execute the flow end to end and return only the final terminal JSON object.

## Caller request

\`\`\`json
{
  "summary": "smoke-blocked",
  "task_type": "coding-task",
  "context": "Real Codex blocked smoke path. There is intentionally no repository task to plan or implement here. Return a blocked terminal outcome rather than inventing work."
}
\`\`\`
EOF

validate_final_json() {
  python3 - <<'PY' "$1"
import json
import pathlib
import sys

payload = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert payload["status"] == "failure", payload
assert payload["failure_cause_type"] == "worker_blocked", payload
assert payload["loop_id"].startswith("loop-"), payload
assert "worktree_ref" in payload, payload
print(json.dumps(payload))
PY
}

transport_fallback_payload() {
  return 1
}

run_transport_fallback() {
  echo "transport fallback is unavailable because the installed skill must not inspect runtime SQLite state directly" >&2
  return 1
}

run_direct_smoke_attempt() {
  local attempt="$1" attempt_log="$LOG_DIR/attempt-$attempt.combined.log" status=0
  set +e
  CODEX_HOME="$CODEX_HOME_DIR" timeout --kill-after=30s "${ATTEMPT_TIMEOUT_SEC}s" codex exec \
    --full-auto \
    --add-dir "$CODEX_HOME_DIR" \
    -c sandbox_workspace_write.network_access=true \
    -c model_reasoning_effort=high \
    -C "$WORKSPACE" \
    -o "$LAST_MESSAGE_FILE" \
    - <"$PROMPT_FILE" 2>&1 | tee "$attempt_log"
  status=${PIPESTATUS[0]}
  set -e
  printf '%s\n' "$status" >"$LOG_DIR/attempt-$attempt.status"
  return "$status"
}

for attempt in 1 2 3; do
  rm -f "$LAST_MESSAGE_FILE"
  attempt_status=0
  if run_direct_smoke_attempt "$attempt"; then
    if validate_final_json "$LAST_MESSAGE_FILE"; then
      printf '%s\n' direct >"$RUN_ROOT/result-source.txt"
      echo "RESULT_SOURCE=direct" >&2
      exit 0
    fi
  else
    attempt_status=$?
    if [[ "$attempt_status" == "124" ]]; then
      echo "real-codex smoke attempt $attempt timed out after ${ATTEMPT_TIMEOUT_SEC}s" >&2
    fi
  fi
  if [[ "$ALLOW_TRANSPORT_FALLBACK" == "1" ]] && run_transport_fallback; then
    printf '%s\n' fallback >"$RUN_ROOT/result-source.txt"
    echo "RESULT_SOURCE=fallback" >&2
    exit 0
  fi
  echo "real-codex smoke attempt $attempt failed; retrying" >&2
  sleep 1
done

[[ -f "$LAST_MESSAGE_FILE" ]] && cat "$LAST_MESSAGE_FILE" >&2
if [[ "$ALLOW_TRANSPORT_FALLBACK" != "1" ]]; then
  echo "transport fallback disabled; set LOOPY_SMOKE_ALLOW_TRANSPORT_FALLBACK=1 to enable it for debugging" >&2
fi
exit 1
