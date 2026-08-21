# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Shanti

Rust CLI tool for creating and managing git worktrees and jujutsu workspaces across multiple repositories to simplify working in multiple concurrent features in separate spaces and being able to manage these spaces easily.

## Tech Stack & Tooling

- Language: Rust
- Library: Ratatui (v0.30+) + Crossterm

## UI & Design Guidelines (Ratatui)

- Avoid generic plain-text or default block layouts.
- Theme: Use a dark-mode palette (e.g., Tokyo Night or Catppuccin Mocha).
- Styling: Leverage Ratatui's `.stylize()` trait heavily. Use bold weights/accent colors for active headers, dimmed/grayed text for secondary details, and high-contrast background highlights for selected items.
- Layout: Always use constraint-based layouts (`Layout::default()`) with strict minimums/percentages so it handles terminal resizes cleanly. Always include a vim-style keybinding footer (e.g., `q: Quit`, `↑/↓: Navigate`).

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

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->

## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:

   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```

5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**

- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->
