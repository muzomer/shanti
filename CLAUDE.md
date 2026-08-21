# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Shanti

Rust CLI tool for creating and managing git worktrees and jujutsu workspaces across multiple repositories to simplify working in multiple concurrent features in separate spaces and being able to manage these spaces easily.

## Build & Test

```bash
cargo build                      # build
cargo test                       # run all tests
cargo test <test_name>           # run a single test by name (substring match)
cargo clippy -- -D warnings      # lint (must pass)
cargo fmt --check                # format check (must pass)
cargo fmt                        # auto-format
```

## Project Structure

```
src/
  main.rs                        # entry point, terminal setup/teardown, event loop
  app.rs                         # App struct: holds all components, routes key events, manages Focus
  cli.rs                         # clap CLI args (--repos-dir, --worktrees-dir, --run-fetch)
  keymap.rs                      # key→Action resolution; InputMode (Normal/Insert)
  github.rs                      # GitHub PR URL parsing, PR info fetching, repo cloning
  lib.rs                         # re-exports
  logs.rs                        # tracing setup
  dirs.rs                        # directory resolution helpers
  git/
    mod.rs                       # public API: list_repositories, worktrees_of_repositories
    repository.rs                # Repository wrapper around git2::Repository; worktree creation, fetch
    worktree.rs                  # Worktree wrapper; delete_worktree
  components/
    mod.rs                       # Action enum, EventState enum, shared style constants
    worktrees.rs                 # WorktreesComponent — main list view (default focus)
    repositories.rs              # RepositoriesComponent — popup for repo selection
    create_worktree.rs           # CreateWorktreeComponent — popup text input for branch name
    confirm.rs                   # ConfirmComponent — generic yes/no confirmation dialog
    help.rs                      # HelpComponent — context-sensitive keybinding help popup
    pr_worktree.rs               # PrWorktreeComponent — popup text input for GitHub PR URL
    list.rs                      # generic list widget used by worktrees/repositories components
    filter.rs                    # filter/search logic for lists
```

## Key Concepts

- **Focus** (`app.rs`): six variants — `Worktrees`, `Repositories`, `CreateWorktree`, `Confirm`, `Help`, `PrWorktree`. Only one has keyboard focus at a time.
- **InputMode** (`keymap.rs`): `Normal` (vi-style nav) or `Insert` (text entry). `keymap::resolve(mode, key)` maps a `KeyEvent` to an `Action`.
- **Action** (`components/mod.rs`): enum of all user intents (e.g. `MoveDown`, `Select`, `Delete`, `OpenPrWorktree`). `App::handle_key` resolves keys to actions then dispatches to the focused component handler.
- **EventState** (`components/mod.rs`): `Consumed`, `NotConsumed`, `Exit`. Components return this from `handle_action` to indicate whether they handled the event.
- **Worktrees** are stored under `SHANTI_WORKTREES_DIR/<repo-name>/<branch-name>/`.
- **Repositories** are discovered by recursively scanning `SHANTI_REPOS_DIR` for `.git` directories.
- **`has_remote_branch`** on `Worktree` indicates whether the local branch has a tracking upstream.
- **GitHub integration** (`github.rs`): `p` opens a PR URL prompt; `P` does the same but auto-creates the worktree. Auth uses `gh` CLI first (with `GITHUB_TOKEN` if set), then falls back to `ureq` + `GITHUB_TOKEN`. If the repo isn't found locally, the user is prompted to clone it via SSH.

## Environment Variables

| Variable               | CLI flag          | Description                           |
| ---------------------- | ----------------- | ------------------------------------- |
| `SHANTI_REPOS_DIR`     | `--repos-dir`     | Directory containing git repositories |
| `SHANTI_WORKTREES_DIR` | `--worktrees-dir` | Directory where worktrees are created |

## Conventions

- Use `color-eyre` for error propagation: `eyre::Result`, `.wrap_err("...")`.
- Use `tracing` macros (`debug!`, `error!`) for logging — no `println!` in library code.
- SSH agent auth is used for git fetch (`Cred::ssh_key_from_agent`). HTTPS auth is not yet implemented (see TODO in `repository.rs`).
- New TUI components should implement `draw(&mut self, frame: &mut Frame, area: Rect)` and `handle_key(&mut self, key: KeyEvent) -> EventState`.
- Tests use `tempfile::tempdir()` for filesystem isolation.
