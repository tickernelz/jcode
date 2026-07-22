#!/usr/bin/env bash
# Merge upstream Jcode changes into the user-owned fork, validate, then optionally push.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/sync_upstream_to_fork.sh [--dry-run|--push]

Fetches 1jehuang/jcode:master read-only into an isolated worktree, merges it
into tickernelz/jcode:master, and validates the merged tree. The default and
--dry-run never publish. --push performs one ordinary, non-force push to the
fork only after validation succeeds.
EOF
}

publish=0
case "${1:-}" in
  ""|--dry-run) ;;
  --push) publish=1 ;;
  -h|--help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"

origin_remote=origin
upstream_remote=upstream
branch=master

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

write_result() {
  [[ -n "${JCODE_SYNC_RESULT_FILE:-}" ]] || return 0
  umask 077
  {
    printf 'changed=%s\n' "$1"
    printf 'base_sha=%s\n' "$2"
    printf 'upstream_sha=%s\n' "$3"
    printf 'tree_sha=%s\n' "$4"
  } >"$JCODE_SYNC_RESULT_FILE"
}

remote_exists() {
  git config --get "remote.$1.url" >/dev/null 2>&1
}

is_fork_url() {
  case "$1" in
    https://github.com/tickernelz/jcode|https://github.com/tickernelz/jcode.git|\
    git@github.com:tickernelz/jcode|git@github.com:tickernelz/jcode.git|\
    ssh://git@github.com/tickernelz/jcode|ssh://git@github.com/tickernelz/jcode.git)
      return 0 ;;
    *) return 1 ;;
  esac
}

is_upstream_url() {
  case "$1" in
    https://github.com/1jehuang/jcode|https://github.com/1jehuang/jcode.git)
      return 0 ;;
    *) return 1 ;;
  esac
}

require_one_url() {
  local description="$1" validator="$2"
  shift 2
  [[ "$#" -eq 1 ]] || fail "$description must have exactly one URL"
  "$validator" "$1" || fail "$description targets an unexpected repository"
}

mapfile -t origin_fetch_raw < <(git config --get-all "remote.$origin_remote.url")
mapfile -t origin_push_raw < <(git config --get-all "remote.$origin_remote.pushurl" || true)
mapfile -t upstream_fetch_raw < <(git config --get-all "remote.$upstream_remote.url")
mapfile -t upstream_push_raw < <(git config --get-all "remote.$upstream_remote.pushurl" || true)

remote_exists "$origin_remote" || fail "missing $origin_remote remote for the user fork"
remote_exists "$upstream_remote" || fail "missing $upstream_remote remote for upstream Jcode"
[[ "${#origin_push_raw[@]}" -gt 0 ]] || origin_push_raw=("${origin_fetch_raw[@]}")

require_one_url "origin fetch" is_fork_url "${origin_fetch_raw[@]}"
require_one_url "origin push" is_fork_url "${origin_push_raw[@]}"
require_one_url "upstream fetch" is_upstream_url "${upstream_fetch_raw[@]}"
[[ "${#upstream_push_raw[@]}" -eq 1 && "${upstream_push_raw[0]}" == "DISABLED" ]] \
  || fail "upstream must have exactly one push URL set to DISABLED"

# Check the URLs after Git applies url.*.insteadOf rules as well. This rejects
# hidden rewrites and multiple push destinations that raw config alone can miss.
mapfile -t origin_fetch_resolved < <(git remote get-url --all "$origin_remote")
mapfile -t origin_push_resolved < <(git remote get-url --push --all "$origin_remote")
mapfile -t upstream_fetch_resolved < <(git remote get-url --all "$upstream_remote")
mapfile -t upstream_push_resolved < <(git remote get-url --push --all "$upstream_remote")
require_one_url "resolved origin fetch" is_fork_url "${origin_fetch_resolved[@]}"
require_one_url "resolved origin push" is_fork_url "${origin_push_resolved[@]}"
require_one_url "resolved upstream fetch" is_upstream_url "${upstream_fetch_resolved[@]}"
[[ "${#upstream_push_resolved[@]}" -eq 1 && "${upstream_push_resolved[0]}" == "DISABLED" ]] \
  || fail "resolved upstream push URL must be DISABLED"

printf 'Fetching fork and upstream %s branches...\n' "$branch"
git fetch --quiet "$origin_remote" \
  "refs/heads/$branch:refs/remotes/$origin_remote/$branch"
# Deliberately omit '+' so a rewritten upstream branch fails closed.
git fetch --quiet "$upstream_remote" \
  "refs/heads/$branch:refs/remotes/$upstream_remote/$branch"

base_sha=$(git rev-parse "$origin_remote/$branch")
upstream_sha=$(git rev-parse "$upstream_remote/$branch")
if git merge-base --is-ancestor "$upstream_remote/$branch" "$origin_remote/$branch"; then
  tree_sha=$(git rev-parse "$origin_remote/$branch^{tree}")
  write_result 0 "$base_sha" "$upstream_sha" "$tree_sha"
  echo "Fork is already up to date with upstream $branch."
  exit 0
fi

scratch_root=${JCODE_SCRATCH_DIR:-${TMPDIR:-/tmp}}
mkdir -p "$scratch_root"
worktree_dir=$(mktemp -d "$scratch_root/jcode-sync.XXXXXX")
rmdir "$worktree_dir"
cleanup() {
  git -C "$repo_dir" worktree remove --force "$worktree_dir" >/dev/null 2>&1 || true
}
trap cleanup EXIT

git worktree add --quiet --detach "$worktree_dir" "$origin_remote/$branch"
cd "$worktree_dir"

printf 'Merging %s/%s into an isolated fork worktree...\n' "$upstream_remote" "$branch"
git merge --no-edit "$upstream_remote/$branch"

echo "Validating merged fork without publishing..."
bash scripts/test_user_owned_update_sources.sh
bash scripts/test_install_conversion.sh
bash scripts/setup_friction_eval.sh
cargo fmt --all -- --check
CARGO_TARGET_DIR="$repo_dir/target" cargo check --workspace

merged_head=$(git rev-parse HEAD)
tree_sha=$(git rev-parse 'HEAD^{tree}')
write_result 1 "$base_sha" "$upstream_sha" "$tree_sha"
if [[ "$publish" != "1" ]]; then
  printf 'Validation passed at %s. The fork was not pushed.\n' "$merged_head"
  exit 0
fi

echo "Verifying ordinary fast-forward push to the user fork..."
git push --dry-run "$origin_remote" "HEAD:refs/heads/$branch"
git push "$origin_remote" "HEAD:refs/heads/$branch"

remote_head=$(git ls-remote "$origin_remote" "refs/heads/$branch" | awk '{print $1}')
[[ -n "$remote_head" && "$merged_head" == "$remote_head" ]] \
  || fail "fork push completed but remote SHA verification failed"
printf 'Synced upstream into tickernelz/jcode at %s.\n' "$merged_head"
