#!/usr/bin/env bash
# Fixtures for create-from-pr.tape only. Shanti's "create space from a PR
# link" flow calls the real GitHub API (or `gh`) and does `git clone
# git@github.com:<owner>/<repo>.git` — there is no offline stand-in for that,
# so unlike setup-local.sh this needs network access, SSH auth, and the
# muzomer/first-repo, muzomer/second-repo, muzomer/third-repo fixture repos
# (with open PRs) to still exist.
set -euo pipefail

DEMOS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPOS_DIR="$DEMOS_DIR/repos-dir"
SPACES_DIR="$DEMOS_DIR/spaces-dir"

rm -rf "$REPOS_DIR" "$SPACES_DIR"
mkdir -p "$REPOS_DIR" "$SPACES_DIR"

for demo_repo in first-repo second-repo; do
  git clone --quiet "git@github.com:muzomer/${demo_repo}.git" "$REPOS_DIR/$demo_repo"
done

echo "GitHub fixtures ready under $REPOS_DIR"
