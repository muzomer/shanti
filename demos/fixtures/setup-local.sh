#!/usr/bin/env bash
# Builds the offline fixtures that demos/*.tape (other than create-from-pr.tape)
# run against: a couple of git repos with a real upstream, and one jj-colocated
# repo. Everything is created locally — no network calls — so demos are
# reproducible without depending on repos under a specific GitHub account.
#
# Mirrors the pattern the test suite uses (tempfile + a local bare repo
# standing in for `origin`; see src/vcs/jj/testing.rs).
set -euo pipefail

DEMOS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPOS_DIR="$DEMOS_DIR/repos-dir"
SPACES_DIR="$DEMOS_DIR/spaces-dir"
REMOTES_DIR="$DEMOS_DIR/.remotes"

rm -rf "$REPOS_DIR" "$SPACES_DIR" "$REMOTES_DIR"
mkdir -p "$REPOS_DIR" "$SPACES_DIR" "$REMOTES_DIR"

make_git_repo() {
  local name="$1"
  local bare="$REMOTES_DIR/$name.git"
  local work="$REPOS_DIR/$name"

  git -c init.defaultBranch=main init --bare --quiet "$bare"
  git clone --quiet "$bare" "$work"
  git -C "$work" config user.email "demo@shanti.dev"
  git -C "$work" config user.name "shanti demo"
  git -C "$work" commit --allow-empty --quiet -m "Initial commit"
  git -C "$work" push --quiet -u origin main
}

# Push one extra commit straight to the bare remote so the working clone
# shows up "behind" — gives the status glyphs something to demonstrate.
make_remote_ahead() {
  local name="$1"
  local bare="$REMOTES_DIR/$name.git"
  local scratch
  scratch="$(mktemp -d)"

  git clone --quiet "$bare" "$scratch"
  git -C "$scratch" config user.email "demo@shanti.dev"
  git -C "$scratch" config user.name "shanti demo"
  git -C "$scratch" commit --allow-empty --quiet -m "A commit that landed upstream"
  git -C "$scratch" push --quiet
  rm -rf "$scratch"
}

make_jj_repo() {
  local name="$1"
  local work="$REPOS_DIR/$name"

  mkdir -p "$work"
  (cd "$work" && jj git init --colocate --quiet && jj describe -m "Initial commit" --quiet)
}

make_git_repo frontend-repo
make_git_repo backend-repo
make_remote_ahead backend-repo

if command -v jj >/dev/null 2>&1; then
  make_jj_repo infra-repo
else
  echo "jj not on PATH — skipping infra-repo (jj) fixture" >&2
fi

echo "Fixtures ready under $REPOS_DIR"
