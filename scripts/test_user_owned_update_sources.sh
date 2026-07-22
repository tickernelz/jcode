#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"

active_sources=(
  crates/jcode-app-core/src/update.rs
  crates/jcode-app-core/src/tool/selfdev/mod.rs
  src/cli/selfdev.rs
  scripts/install.sh
  scripts/install.ps1
  scripts/generate_release_notes.sh
  scripts/update_packages.sh
  .github/workflows/release.yml
)

if grep -En '1jehuang/jcode|jcode\.sh/releases' "${active_sources[@]}"; then
  echo "active update paths must not reference upstream or external release metadata" >&2
  exit 1
fi

grep -q 'const GITHUB_REPO: &str = "tickernelz/jcode"' crates/jcode-app-core/src/update.rs
grep -q 'const GITHUB_BRANCH: &str = "master"' crates/jcode-app-core/src/update.rs
grep -q 'refs/heads/master:refs/remotes/origin/master' crates/jcode-app-core/src/update.rs
grep -q 'configure_source_remote(&repo_dir)' crates/jcode-app-core/src/update.rs
grep -q 'JCODE_REPO_URL: &str = "https://github.com/tickernelz/jcode.git"' crates/jcode-app-core/src/tool/selfdev/mod.rs
grep -q 'JCODE_REPO_URL: &str = "https://github.com/tickernelz/jcode.git"' src/cli/selfdev.rs
grep -q 'REPO="tickernelz/jcode"' scripts/install.sh
grep -q '\$Repo = "tickernelz/jcode"' scripts/install.ps1
grep -q 'REPO="tickernelz/jcode"' scripts/generate_release_notes.sh
grep -q 'https://github.com/tickernelz/jcode/releases/download' scripts/update_packages.sh
grep -q 'https://github.com/tickernelz/jcode/releases/download' .github/workflows/release.yml

notes=$(GITHUB_REPOSITORY=1jehuang/jcode \
  bash scripts/generate_release_notes.sh v999.0.0)
grep -q 'https://github.com/tickernelz/jcode/commits/v999\.0\.0' <<<"$notes"
if grep -q 'https://github.com/1jehuang/jcode/' <<<"$notes"; then
  echo "release notes must ignore legacy repository overrides" >&2
  exit 1
fi

echo "user-owned update source tests passed"
