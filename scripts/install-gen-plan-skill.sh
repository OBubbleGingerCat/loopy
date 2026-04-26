#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
CALLER_CWD="$(pwd -P)"

TARGET_MODE="codex"
TARGET_EXPLICIT=0
POSITIONAL_PATH=""
INSTALL_ROOT=""
PATH_EXPLICIT=0
CUSTOM_INSTALL_ROOT=0

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
      [[ "$TARGET_EXPLICIT" == "0" ]] || {
        echo "conflicting install selectors: --target specified more than once" >&2
        usage >&2
        exit 1
      }
      TARGET_MODE="$2"
      TARGET_EXPLICIT=1
      shift 2
      ;;
    --path)
      [[ $# -ge 2 ]] || { echo "missing value for --path" >&2; exit 1; }
      [[ "$PATH_EXPLICIT" == "0" ]] || {
        echo "conflicting install selectors: --path specified more than once" >&2
        usage >&2
        exit 1
      }
      [[ -n "$2" ]] || {
        echo "empty install root is not allowed for --path" >&2
        usage >&2
        exit 1
      }
      INSTALL_ROOT="$2"
      PATH_EXPLICIT=1
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
        [[ -n "$1" ]] || {
          echo "empty positional install root is not allowed" >&2
          usage >&2
          exit 1
        }
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

if [[ -n "$INSTALL_ROOT" && -n "$POSITIONAL_PATH" ]]; then
  echo "conflicting install selectors: cannot combine --path with a positional install root" >&2
  usage >&2
  exit 1
fi

if [[ "$TARGET_EXPLICIT" == "1" && ( -n "$INSTALL_ROOT" || -n "$POSITIONAL_PATH" ) ]]; then
  echo "conflicting install selectors: cannot combine --target with a custom install root" >&2
  usage >&2
  exit 1
fi

normalize_install_root() {
  local raw_path="$1"
  local absolute_path
  absolute_path=""
  case "$raw_path" in
    /*) absolute_path="$raw_path" ;;
    *) absolute_path="$CALLER_CWD/$raw_path" ;;
  esac
  canonicalize_path "$absolute_path"
}

canonicalize_path() {
  local raw_path="$1"
  local -a segments=()
  local -a collapsed=()
  local segment
  IFS='/' read -r -a segments <<< "$raw_path"
  for segment in "${segments[@]}"; do
    case "$segment" in
      ""|".")
        continue
        ;;
      "..")
        if [[ "${#collapsed[@]}" -gt 0 ]]; then
          unset 'collapsed[${#collapsed[@]}-1]'
        fi
        ;;
      *)
        collapsed+=("$segment")
        ;;
    esac
  done
  if [[ "${#collapsed[@]}" -eq 0 ]]; then
    printf '/\n'
  else
    local IFS='/'
    printf '/%s\n' "${collapsed[*]}"
  fi
}

resolve_real_install_root() {
  local raw_path="$1"
  local current="$raw_path"
  local -a suffix=()
  local part
  while [[ ! -e "$current" ]]; do
    suffix=("$(basename "$current")" "${suffix[@]}")
    current="$(dirname "$current")"
  done

  [[ -d "$current" ]] || {
    echo "unsafe install root: existing ancestor is not a directory" >&2
    exit 1
  }

  local resolved
  resolved="$(cd -P "$current" && pwd -P)"
  for part in "${suffix[@]}"; do
    resolved="$resolved/$part"
  done
  canonicalize_path "$resolved"
}

path_overlaps() {
  local left="$1"
  local right="$2"
  [[ "$left" == "$right" || "$left" == "$right"/* || "$right" == "$left"/* ]]
}

assert_safe_install_root() {
  local install_root="$1"
  local install_root_real
  local caller_parent_real
  install_root_real="$(resolve_real_install_root "$install_root")"
  caller_parent_real="$(resolve_real_install_root "$CALLER_CWD/..")"

  if [[ "$CUSTOM_INSTALL_ROOT" == "1" && "$(basename "$install_root")" != "loopy-gen-plan" ]]; then
    echo "unsafe install root: custom install roots must end with loopy-gen-plan" >&2
    exit 1
  fi

  if [[ "$install_root_real" == "/" ]]; then
    echo "unsafe install root: refusing to install into /" >&2
    exit 1
  fi

  if [[ "$install_root_real" == "$CALLER_CWD" || "$install_root_real" == "$caller_parent_real" ]]; then
    echo "unsafe install root: refusing to install into the caller working directory or its parent" >&2
    exit 1
  fi

  if path_overlaps "$install_root_real" "$REPO_ROOT"; then
    echo "unsafe install root: destination must not overlap the repository" >&2
    exit 1
  fi
}

resolve_target_install_root() {
  case "$TARGET_MODE" in
    codex)
      if [[ -n "${CODEX_HOME:-}" ]]; then
        printf '%s/skills/loopy-gen-plan\n' "$CODEX_HOME"
      else
        [[ -n "${HOME:-}" ]] || {
          echo "HOME is required when CODEX_HOME is not set for the codex install target" >&2
          exit 1
        }
        printf '%s/.codex/skills/loopy-gen-plan\n' "$HOME"
      fi
      ;;
    claude)
      [[ -n "${HOME:-}" ]] || {
        echo "HOME is required for the claude install target" >&2
        exit 1
      }
      printf '%s/.claude/skills/loopy-gen-plan\n' "$HOME"
      ;;
    *)
      echo "unknown installer target: $TARGET_MODE" >&2
      usage >&2
      exit 1
      ;;
  esac
}

if [[ -n "$POSITIONAL_PATH" ]]; then
  CUSTOM_INSTALL_ROOT=1
  INSTALL_ROOT="$(normalize_install_root "$POSITIONAL_PATH")"
elif [[ -n "$INSTALL_ROOT" ]]; then
  CUSTOM_INSTALL_ROOT=1
  INSTALL_ROOT="$(normalize_install_root "$INSTALL_ROOT")"
else
  INSTALL_ROOT="$(resolve_target_install_root)"
fi

INSTALL_ROOT="$(normalize_install_root "$INSTALL_ROOT")"
assert_safe_install_root "$INSTALL_ROOT"

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
cp "$SOURCE_ROOT/prompts/refine_instructions.md" \
  "$INSTALL_ROOT/prompts/refine_instructions.md"
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
