#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_MODE="codex"
POSITIONAL_PATH=""
INSTALL_ROOT=""

usage() {
  cat <<'EOF'
Usage:
  scripts/install-submit-loop-skill.sh --target codex
  scripts/install-submit-loop-skill.sh --target claude
  scripts/install-submit-loop-skill.sh --path /custom/location/loopy-submit-loop
  scripts/install-submit-loop-skill.sh /custom/location/loopy-submit-loop
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      [[ $# -ge 2 ]] || { echo "missing value for --target" >&2; exit 1; }
      TARGET_MODE="$2"
      shift 2
      ;;
    --path)
      [[ $# -ge 2 ]] || { echo "missing value for --path" >&2; exit 1; }
      INSTALL_ROOT="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      if [[ -z "$POSITIONAL_PATH" ]]; then
        POSITIONAL_PATH="$1"
        shift
      else
        echo "unexpected argument: $1" >&2
        usage >&2
        exit 1
      fi
      ;;
  esac
done

if [[ -n "$POSITIONAL_PATH" ]]; then
  INSTALL_ROOT="$POSITIONAL_PATH"
elif [[ -z "$INSTALL_ROOT" ]]; then
  case "$TARGET_MODE" in
    codex)
      INSTALL_ROOT="${CODEX_HOME:-$HOME/.codex}/skills/loopy-submit-loop"
      ;;
    claude)
      INSTALL_ROOT="$HOME/.claude/skills/loopy-submit-loop"
      ;;
    *)
      echo "unknown installer target: $TARGET_MODE" >&2
      usage >&2
      exit 1
      ;;
  esac
fi

BUILD_PROFILE="${CARGO_BUILD_PROFILE:-debug}"
BUILD_FLAGS=()
if [[ "$BUILD_PROFILE" == "release" ]]; then
  BUILD_FLAGS+=(--release)
fi
if [[ "${CARGO_NET_OFFLINE:-}" == "true" || "${CARGO_NET_OFFLINE:-}" == "1" ]]; then
  BUILD_FLAGS+=(--offline)
fi

cd "$REPO_ROOT"
cargo build "${BUILD_FLAGS[@]}" -p loopy-submit-loop --bin loopy-submit-loop

BIN_PATH="$REPO_ROOT/target/$BUILD_PROFILE/loopy-submit-loop"
if [[ ! -x "$BIN_PATH" ]]; then
  echo "expected built binary at $BIN_PATH" >&2
  exit 1
fi

SOURCE_ROOT="$REPO_ROOT/skills/submit-loop"

rm -rf "$INSTALL_ROOT"
mkdir -p \
  "$INSTALL_ROOT/bin" \
  "$INSTALL_ROOT/roles" \
  "$INSTALL_ROOT/roles/coding-task/planning_worker" \
  "$INSTALL_ROOT/roles/coding-task/artifact_worker" \
  "$INSTALL_ROOT/roles/coding-task/checkpoint_reviewer" \
  "$INSTALL_ROOT/roles/coding-task/artifact_reviewer"

cp "$SOURCE_ROOT/SKILL.md" "$INSTALL_ROOT/SKILL.md"
cp "$SOURCE_ROOT/coordinator.md" "$INSTALL_ROOT/coordinator.md"
cp "$SOURCE_ROOT/submit-loop.toml" "$INSTALL_ROOT/submit-loop.toml"
cp "$SOURCE_ROOT/bundle.toml" "$INSTALL_ROOT/bundle.toml"
cp "$SOURCE_ROOT/roles/coding-task/task-type.toml" \
  "$INSTALL_ROOT/roles/coding-task/task-type.toml"
cp "$SOURCE_ROOT/roles/coding-task/planning_worker/codex_planner.md" \
  "$INSTALL_ROOT/roles/coding-task/planning_worker/codex_planner.md"
cp "$SOURCE_ROOT/roles/coding-task/planning_worker/mock_planner.md" \
  "$INSTALL_ROOT/roles/coding-task/planning_worker/mock_planner.md"
cp "$SOURCE_ROOT/roles/coding-task/artifact_worker/codex_implementer.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_worker/codex_implementer.md"
cp "$SOURCE_ROOT/roles/coding-task/artifact_worker/mock_implementer.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_worker/mock_implementer.md"
cp "$SOURCE_ROOT/roles/coding-task/checkpoint_reviewer/codex_scope.md" \
  "$INSTALL_ROOT/roles/coding-task/checkpoint_reviewer/codex_scope.md"
cp "$SOURCE_ROOT/roles/coding-task/checkpoint_reviewer/codex_plan.md" \
  "$INSTALL_ROOT/roles/coding-task/checkpoint_reviewer/codex_plan.md"
cp "$SOURCE_ROOT/roles/coding-task/checkpoint_reviewer/codex_contract.md" \
  "$INSTALL_ROOT/roles/coding-task/checkpoint_reviewer/codex_contract.md"
cp "$SOURCE_ROOT/roles/coding-task/checkpoint_reviewer/mock.md" \
  "$INSTALL_ROOT/roles/coding-task/checkpoint_reviewer/mock.md"
cp "$SOURCE_ROOT/roles/coding-task/artifact_reviewer/codex_checkpoint_contract.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_reviewer/codex_checkpoint_contract.md"
cp "$SOURCE_ROOT/roles/coding-task/artifact_reviewer/codex_correctness.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_reviewer/codex_correctness.md"
cp "$SOURCE_ROOT/roles/coding-task/artifact_reviewer/codex_code_quality.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_reviewer/codex_code_quality.md"
cp "$SOURCE_ROOT/roles/coding-task/artifact_reviewer/mock.md" \
  "$INSTALL_ROOT/roles/coding-task/artifact_reviewer/mock.md"
cp "$BIN_PATH" "$INSTALL_ROOT/bin/loopy-submit-loop"

chmod +x "$INSTALL_ROOT/bin/loopy-submit-loop"

printf '%s\n' "$INSTALL_ROOT"
