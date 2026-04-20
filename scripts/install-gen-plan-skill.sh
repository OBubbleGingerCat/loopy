#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CALLER_CWD="$(pwd)"

TARGET_MODE="codex"
POSITIONAL_PATH=""
INSTALL_ROOT=""

usage() {
  cat <<'EOF'
Usage:
  scripts/install-gen-plan-skill.sh
  scripts/install-gen-plan-skill.sh --target codex
  scripts/install-gen-plan-skill.sh --target claude
  scripts/install-gen-plan-skill.sh --path /custom/location/loopy-gen-plan
  scripts/install-gen-plan-skill.sh /custom/location/loopy-gen-plan

With no arguments, installs to the default Codex skill location.
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
    --*)
      echo "unexpected flag: $1" >&2
      usage >&2
      exit 1
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

normalize_install_root() {
  local raw_path="$1"
  case "$raw_path" in
    /*) printf '%s\n' "$raw_path" ;;
    *) printf '%s\n' "$CALLER_CWD/$raw_path" ;;
  esac
}

if [[ -n "$POSITIONAL_PATH" ]]; then
  INSTALL_ROOT="$(normalize_install_root "$POSITIONAL_PATH")"
elif [[ -n "$INSTALL_ROOT" ]]; then
  INSTALL_ROOT="$(normalize_install_root "$INSTALL_ROOT")"
else
  case "$TARGET_MODE" in
    codex)
      INSTALL_ROOT="${CODEX_HOME:-$HOME/.codex}/skills/loopy-gen-plan"
      ;;
    claude)
      INSTALL_ROOT="$HOME/.claude/skills/loopy-gen-plan"
      ;;
    *)
      echo "unknown installer target: $TARGET_MODE" >&2
      usage >&2
      exit 1
      ;;
  esac
fi

INSTALL_ROOT="$(normalize_install_root "$INSTALL_ROOT")"

BUILD_PROFILE="${CARGO_BUILD_PROFILE:-debug}"
BUILD_FLAGS=()
if [[ "$BUILD_PROFILE" == "release" ]]; then
  BUILD_FLAGS+=(--release)
fi
if [[ "${CARGO_NET_OFFLINE:-}" == "true" || "${CARGO_NET_OFFLINE:-}" == "1" ]]; then
  BUILD_FLAGS+=(--offline)
fi

cd "$REPO_ROOT"
cargo build "${BUILD_FLAGS[@]}" -p loopy-gen-plan --bin loopy-gen-plan

BIN_PATH="$REPO_ROOT/target/$BUILD_PROFILE/loopy-gen-plan"
if [[ ! -x "$BIN_PATH" ]]; then
  echo "expected built binary at $BIN_PATH" >&2
  exit 1
fi

SOURCE_ROOT="$REPO_ROOT/skills/gen-plan"

rm -rf "$INSTALL_ROOT"
mkdir -p \
  "$INSTALL_ROOT/bin" \
  "$INSTALL_ROOT/prompts" \
  "$INSTALL_ROOT/roles/coding-task/leaf_reviewer" \
  "$INSTALL_ROOT/roles/coding-task/frontier_reviewer"

cp "$SOURCE_ROOT/SKILL.md" "$INSTALL_ROOT/SKILL.md"
cp "$SOURCE_ROOT/bundle.toml" "$INSTALL_ROOT/bundle.toml"
cp "$SOURCE_ROOT/gen-plan.toml" "$INSTALL_ROOT/gen-plan.toml"
cp "$SOURCE_ROOT/prompts/domain_contract.md" \
  "$INSTALL_ROOT/prompts/domain_contract.md"
cp "$SOURCE_ROOT/prompts/leaf_runtime.md" \
  "$INSTALL_ROOT/prompts/leaf_runtime.md"
cp "$SOURCE_ROOT/prompts/frontier_runtime.md" \
  "$INSTALL_ROOT/prompts/frontier_runtime.md"
cp "$SOURCE_ROOT/roles/coding-task/task-type.toml" \
  "$INSTALL_ROOT/roles/coding-task/task-type.toml"
cp "$SOURCE_ROOT/roles/coding-task/leaf_reviewer/codex_default.md" \
  "$INSTALL_ROOT/roles/coding-task/leaf_reviewer/codex_default.md"
cp "$SOURCE_ROOT/roles/coding-task/leaf_reviewer/mock.md" \
  "$INSTALL_ROOT/roles/coding-task/leaf_reviewer/mock.md"
cp "$SOURCE_ROOT/roles/coding-task/frontier_reviewer/codex_default.md" \
  "$INSTALL_ROOT/roles/coding-task/frontier_reviewer/codex_default.md"
cp "$SOURCE_ROOT/roles/coding-task/frontier_reviewer/mock.md" \
  "$INSTALL_ROOT/roles/coding-task/frontier_reviewer/mock.md"
cp "$BIN_PATH" "$INSTALL_ROOT/bin/loopy-gen-plan"

chmod +x "$INSTALL_ROOT/bin/loopy-gen-plan"

printf '%s\n' "$INSTALL_ROOT"
