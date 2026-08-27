# CLAUDE.md

Orientation for coding agents working in this repository.

# Shanti

Rust TUI for creating and managing **spaces** — git worktrees and jujutsu workspaces — across many repositories, so several concurrent features or PRs each get their own checked-out directory that is easy to switch between.

Three files, three jobs, no overlap:

- **`README.md`** — the tool for the person _using_ it: features, keybindings,
  configuration, environment variables, status glyphs.
- **`CONTEXT.md`** — the vocabulary. What a *space*, a *backend* or an *inbox
  item* means here. Use those words; the code is named after them.
- **this file** — how to build, test and write code that fits in.

How the source is arranged is deliberately written down nowhere: it is
discoverable from the tree, and a description of it in prose is a description
that goes stale.

## Tech Stack & Tooling

- Language: Rust
- UI: Ratatui (v0.30+) + Crossterm
- git: the `git2` library. jujutsu: the `jj` **command-line tool** (never a linked library).

## UI & Design Guidelines

- Avoid generic plain-text or default block layouts.
- **Colour is chosen in one place, and by meaning.** A component asks for the
  role it is drawing — a title, a muted detail, a selected row, a destructive
  border — never for a hue. That is what lets the user change scheme while the
  app is running and have the next frame repaint.
- Bold accent colours for active headers, dimmed text for secondary detail, a
  high-contrast band for the selected row.
- Constraint-based layouts with strict minimums, so resizing degrades in a way
  that was chosen rather than clipped. Content that no longer fits is hidden,
  not cut in half.
- Every screen carries a vim-style keybinding footer.

## Build & Test

```bash
cargo build                      # build
cargo test                       # run all tests
cargo test <test_name>           # run a single test by name (substring match)
cargo clippy -- -D warnings      # lint (must pass)
cargo fmt --check                # format check (must pass)
cargo fmt                        # auto-format
```

The jj tests need a real `jj` (0.28.0+) on `PATH`; when it is missing the
fixtures in `src/vcs/jj/testing.rs` make those tests **skip**, not fail.

## Conventions

- Use `color-eyre` for error propagation: `eyre::Result`, `.wrap_err("...")`.
- Use `tracing` macros (`debug!`, `error!`) for logging — no `println!` in library
  code. `main.rs` is the only place that writes to stdout (the selected path,
  which `cd $(shanti)` consumes) or stderr.
- Comment the _why_ — intent, trade-offs, gotchas — not the _what_. The house
  style is a module-level doc comment stating the rule the module exists to
  enforce.
- A failed fetch costs a stale view of the remotes and nothing else: never drop a
  repository from the list or abort a flow because of it.
- git fetch authenticates through the **SSH agent** only; HTTPS credentials are
  not implemented.
- A new popup is a new `Modal` implementation, not another field on `App`.
- Tests use `tempfile::tempdir()` for filesystem isolation, and a local _bare_
  repository stands in for `origin` so the suite runs offline.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**

```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**

- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

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
