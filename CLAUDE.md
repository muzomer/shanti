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

## Issue tracking

This project uses **bd (beads)**. See the `beads` skill for the full workflow.
