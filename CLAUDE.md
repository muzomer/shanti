# CLAUDE.md

This file provides guidance to coding agents (Claude Code, and anything reading `AGENTS.md`) when working with code in this repository.

# Shanti

Rust TUI for creating and managing **spaces** — git worktrees and jujutsu workspaces — across many repositories, so several concurrent features or PRs each get their own checked-out directory that is easy to switch between.

`README.md` documents the tool for the person _using_ it (features, keybindings, configuration, status glyphs). This file is orientation for the person _editing_ it; read the README once for the user-facing behaviour instead of re-deriving it here.

## Tech Stack & Tooling

- Language: Rust
- UI: Ratatui (v0.30+) + Crossterm
- git: the `git2` library. jujutsu: the `jj` **command-line tool** (never a linked library).

## UI & Design Guidelines (Ratatui)

- Avoid generic plain-text or default block layouts.
- **Colour lives in `src/theme/` and nowhere else.** No component names a raw
  `Color`, and none names a constant either: it calls the accessor for the
  _meaning_ (`theme::title()`, `theme::muted()`, `theme::selected_row()`,
  `theme::border_destructive()`, …). The palette is a runtime `Theme` value held
  in one process-global slot — `theme::set` is the only mutation point, and
  every accessor reads the slot, so a scheme can change while the process runs.
  `theme::tone(Tone)` is the single mapping from a domain `Tone` to a colour.
  The hues themselves live only in the scheme modules (`theme/tokyo_night.rs`,
  `theme/catppuccin.rs`, `theme/gruvbox.rs`, `theme/ansi.rs`), each one a
  `Theme` constructor, catalogued in `theme/scheme.rs`. Tokyo Night (night
  variant) is the default; adding a scheme means adding a constructor and an
  entry in `scheme::ALL`, never touching a component.
- Styling: bold accent colours for active headers, dimmed text for secondary
  detail, a high-contrast background band for the selected row.
- Layout: constraint-based layouts (`Layout`) with strict minimums/percentages so
  resizes behave. Always include a vim-style keybinding footer (e.g. `q: Quit`,
  `↑/↓: Navigate`).

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

## Project Structure

```
src/
  main.rs                        # entry point: terminal setup/teardown; prints the selected path on stdout
  lib.rs                         # run_app (draw + event loop) and Outcome::{Selected, Quit}
  app.rs                         # App: the space list plus a stack of modals; routes keys to actions
  cli.rs                         # clap args and the configuration precedence rules
  config.rs                      # the TOML configuration file (~/.config/shanti/config.toml)
  events.rs                      # AppEvent / EventSource: keys, paste, ticks and jobs on one channel
  jobs/mod.rs                    # Job / JobResult / Worker: the bounded pool that runs slow work
  keymap.rs                      # key → Action resolution; InputMode (Normal/Insert)
  theme/
    mod.rs                       # the one place a colour is chosen: Theme, the global slot, set(), accessors, tone()
    scheme.rs                    # the catalogue: Scheme { name, label, appearance }, ALL, DEFAULT, find/theme
    tokyo_night.rs               # tokyo-night (default), tokyo-night-storm, tokyo-night-day
    catppuccin.rs                # catppuccin-mocha, catppuccin-latte
    gruvbox.rs                   # gruvbox-dark
    ansi.rs                      # ansi: the terminal's own 16 colours, no hue of its own
  github.rs                      # PR URL parsing, PrFetcher, PR lookup, repository cloning
  hooks.rs                       # post-create hooks: HookSettings -> HookPlan -> HookReport
  logs.rs                        # tracing setup
  space_meta.rs                  # what shanti remembers about a space: the PR it came from
  dirs.rs                        # data/config directory resolution
  components/
    mod.rs                       # Action and EventState enums; re-exports
    modal.rs                     # Modal trait, ModalFlow, AppContext, Confirm/SelectCallback, centered()
    worktrees.rs                 # WorktreesComponent — the spaces pane (SpaceEntry rows), repo-scoped in two-pane
    repositories.rs              # RepositoriesComponent (owns the backends): the left pane and the fallback picker modal
    create_worktree.rs           # popup text input for a new space name
    pr_worktree.rs               # popup text input for a GitHub PR URL, and the flow behind it
    select_directory.rs          # popup to pick one of the repos dirs (used when cloning)
    theme_picker.rs              # ThemeModal: the colour scheme picker, previewing each scheme live
    confirm.rs                   # generic yes/no dialog, deferring the "yes" to a callback
    help.rs                      # context-sensitive keybinding help popup; HelpEntry
    notify.rs                    # notifications: severity, expiry, and the single message slot
    list.rs                      # generic list widget shared by the list views
    filter.rs                    # filter/search input state
  vcs/
    mod.rs                       # the Vcs trait, BoxedVcs, open_backends/open_at, refresh, space_dest
    backend.rs                   # Backend enum (Git | Jj) with its labels and nouns
    repo.rs                      # Repo and RepoId
    space.rs                     # Space — the backend-neutral unit of work
    status.rs                    # SpaceStatus, RemoteState, LocalState, JjLocal, StatusGlyph, Tone
    delete.rs                    # DeletionRisk / Consequence: what deleting a space would cost
    discover.rs                  # the on-disk walk; Discovered, backend_at, backends_at
    git/                         # the git backend: mod.rs, backend.rs (GitBackend), worktree.rs
    jj/                          # the jj backend: backend.rs (JjBackend), cmd.rs (JjCli), base.rs,
                                 #   status.rs, template.rs, version.rs, workspace.rs, testing.rs
tests/
  common/mod.rs                  # fixtures shared by the suites: git, init_repo, boot/booting, keys
  state.rs                       # App state machine driven through real key events
  render.rs                      # what a frame actually paints, asserted cell by cell
  git_backend.rs                 # git backend end to end, against real repositories on disk
  jj_backend.rs                  # jj backend end to end (skips when jj is missing)
  colocated.rs                   # a repository with both .git and .jj
docs/adr/                        # decisions that are hard to reverse; read before contradicting one
```

## Key Concepts

### The VCS seam

- **`Vcs`** (`vcs/mod.rs`) is the single seam every backend implements: `spaces`,
  `create_space`, `delete_space`, `fetch`, `resolve_base`, `repo`, `backend`. It
  is deliberately **object-safe** — git and jj repositories live in one
  collection — so backends are stored as **`BoxedVcs`** (`Box<dyn Vcs>`). No
  generic methods, no `async`, no `Self` in return position.
- Everything in `vcs/` is an **owned snapshot**: no type holds a
  `git2::Repository`, a file handle or a child process, so lists survive across
  frames and can move between threads.
- **`Space`** is the backend-neutral word for one checked-out directory — a git
  _worktree_ or a jj _workspace_. The UI holds `Space`, never a git-specific
  type. `Backend::space_noun()` supplies the backend's own word for messages.
- **Layout policy lives in one place**: `vcs::space_dest(worktrees_dir, repo, name)`
  → `<worktrees dir>/<repo-name>/<space-name>`. `create_space` takes the
  destination rather than deriving it, so no backend can drift from it.
- **`vcs::discover`** walks the repos dirs cheaply (`stat` only, bounded depth,
  the worktrees dir excluded) and decides the backend from the layout: `.jj` → jj,
  `.git` only → git, **both (colocated) → jj owns it**, with git kept as an
  _additional_ backend so pre-existing git worktrees are still listed. Only the
  owner failing to open is fatal.
- **`SpaceStatus`** (`vcs/status.rs`) renders as **two glyphs**: the remote half
  (`RemoteState`) means the same for a git branch and a jj bookmark; the local
  half genuinely differs (git: dirty working tree; jj: empty / conflicted /
  divergent, since jj auto-commits). Each `StatusGlyph` carries a symbol, a
  plain-English `meaning`, and a semantic **`Tone`**
  (`Muted`/`Ok`/`Info`/`Warn`/`Danger`) — the domain model never names a colour.
- **`SpaceTip`** (`vcs/tip.rs`) is the last commit — or jj change — made in a
  space: a subject and a Unix timestamp, read by each backend while it is
  already listing (git peels the branch it has open, jj adds two fields to the
  workspace template), so no space costs an extra round trip. The age is derived
  at render time, never stored: a snapshot holding "3 hours ago" is wrong the
  moment it is taken.
- **`vcs::delete`** turns a `SpaceStatus` into a `DeletionRisk`, so the UI can
  pick a proportionate confirmation. Safety is decided by
  `SpaceStatus::has_unsaved_work` / `RemoteState::has_unpushed_work` and never
  re-derived elsewhere.
- **jj is driven through the CLI.** `vcs/jj/cmd.rs` (`JjCli`) is the only place in
  the codebase that spawns a process; it guarantees no pager, no colour, an
  explicit repository, a checked version floor (0.28.0) and template-based,
  machine-readable output. Never link `jj_lib`, and never parse jj's
  human-facing output — add a template in `vcs/jj/template.rs` instead.
- Likewise, `vcs/git/` is the only place allowed to name a `git2` type.

### Post-create hooks

- **`hooks.rs`** is the whole feature: `HookSettings` (the configured hooks, from
  `cli::Args`) → `HookSettings::plan(target)` (pure, cheap, safe on the render
  thread) → `HookPlan::run()` (**blocking** — only ever on a worker) →
  `HookReport`. `HookReport::summary()` is `None` when nothing failed, because a
  hook that works must be invisible.
- `vcs::create_space_with_hooks` is the only way a space is created in the app:
  creating and planning happen together so no flow can create a space and forget
  its setup. `AppContext::create_space` leaves the plan in `pending_hooks`, and
  `App::pump_hooks` submits it as `Job::RunHooks` once the modal stack settles —
  an empty plan is never submitted, so a user with no hooks pays nothing.
- A failing hook never destroys or rolls back the space, and hooks are only ever
  read from the user's own configuration file — never from a repository's
  working tree.

### The UI

- **Two-pane layout** (`app.rs::draw`): repositories on the left, the
  highlighted repository's spaces on the right. `App` holds a `Pane` focus
  (`Repositories` | `Spaces`); `Tab` switches panes, `i`/`/` enter the focused
  pane's filter, and `n` creates a space in the highlighted repository with no
  picker. The spaces list is one global list narrowed to the selected repository
  by `WorktreesComponent::set_repo_scope`. Below `two_pane_fits` (the two pane
  minimums combined) the layout folds to the single spaces list, and `n` falls
  back to the `RepositoriesModal` picker — the only code path that survives from
  the pre-two-pane flow. `two_pane` is decided in `draw` (the only place with a
  terminal size) and read by the key handlers, exactly as the loop draws before
  every input.
- **Modal stack** (`app.rs` + `components/modal.rs`): there is no `Focus` enum —
  focus is the `Pane` above plus "whatever is on top of the stack". `App` holds
  `modals: Vec<Box<dyn Modal>>` over the panes. Drawing walks the stack
  bottom-to-top; the effective `InputMode` is the top modal's, so popping
  restores the layer below by construction. A modal returns a **`ModalFlow`**
  (`Consumed`, `Ignored`, `Close`, `Replace(next)`) saying what should happen to
  the stack — `App` never learns what a modal is _for_. Each modal names its
  identity with `Modal::kind() -> ModalKind`, so the stack can be observed
  (`App::top_modal`, `App::modal_depth`) without reading what it paints. Adding a
  popup means adding a `Modal` implementation, not another field on `App`.
- **`AppContext`** is the state lent to a modal while it runs (the space list,
  the repositories, the resolved `cli::Args`) plus shared work such as
  `create_space`. `ConfirmCallback` / `SelectCallback` let a generic dialog defer
  the meaning of "yes" to whoever opened it.
- **`InputMode`** (`keymap.rs`): `Normal` (vi-style nav) or `Insert` (text entry).
  `keymap::resolve(mode, key)` maps a `KeyEvent` to an `Action`.
- **`Action`** (`components/mod.rs`): every user intent (`MoveDown`, `Select`,
  `Delete`, `OpenPrWorktree`, …). `App::handle_key` resolves the key, then
  dispatches to the top modal or to the list.
- **`EventState`** (`components/mod.rs`): `Consumed`, `NotConsumed`, `Exit`.
  Re-exported from `lib.rs` (with `ModalKind` and `InputMode`) so integration
  tests can name the type `App::handle_key` returns and assert on the modal
  stack, rather than matching Debug strings.
- **`events.rs`**: terminal input, bracketed paste, a periodic tick and (later)
  background jobs all arrive on **one channel** (`EventSource` → `AppEvent`), so
  the loop never blocks on any single source and can redraw while nothing is
  typed. Dropping `EventSource` stops its producer threads and joins them.
- **`App::with_args(args, pr_fetcher)`** is the construction seam: it reads no
  argv, no environment and no configuration file, so tests can point an `App` at
  their own temp directories and their own PR lookup without disturbing anything
  else. `App::new()` is the thin wrapper that resolves the real configuration.
- **The colour scheme** is chosen once at startup and then at will: `main.rs`
  installs `args.theme` with `theme::set`, and `t` opens `ThemeModal`
  (`components/theme_picker.rs`), which _is_ the preview — moving the cursor
  calls `theme::set`, so the next frame repaints everything in the highlighted
  scheme. `Enter` persists the name through `config::persist_theme` (the only
  writer of the user's configuration file, rewriting the single `theme` key);
  `Esc` restores the `Theme` value the modal captured when it opened. A failed
  write is a notification, never a rollback: the scheme stays active for the run.
- **The detail pane** (`components/detail.rs`) is a plain function, not a modal:
  it draws the highlighted space's tip, remote counts, local state, path and PR
  from the snapshot the list already holds, so moving the cursor costs a
  re-render and no I/O. `app.rs::split_off_detail` decides whether it appears —
  the list keeps `MIN_HEIGHT` first, and below that the pane is hidden rather
  than clipped. The keybinding footer lives on the spaces list's own bottom
  border, which is now above the pane rather than at the screen's edge.
- **What shanti remembers** (`space_meta.rs`): a space made from a pull request
  came from a URL that no backend records. `SpaceMeta` keeps it in
  `<data dir>/spaces.toml` — a **cache**, not a source of truth: an unreadable
  file reads as "remember nothing", a failed write never fails the space
  creation, and entries whose directory is gone are dropped on the next write.
  `AppContext::remember_pr` is how a flow records one.
- **GitHub integration** (`github.rs`): `p` opens a PR URL prompt; `P` does the
  same and clones the repository first if it is missing. Lookups go through
  `PrFetcher` (`Arc<dyn Fn(&PrUrl) -> Result<PrInfo>>`) — `github::live_fetcher()`
  in production, a stub in tests. The live one uses the `gh` CLI when available,
  otherwise HTTPS with `GITHUB_TOKEN`. A clone made this way is always a plain
  git clone.

## Configuration

Four layers, later wins: built-in defaults → configuration file → environment
variables → command line flags. `cli.rs` decides the precedence and records
where each value came from (`--show-config` prints the winner and its origin).

| Variable               | CLI flag          | Description                                                      |
| ---------------------- | ----------------- | ---------------------------------------------------------------- |
| `SHANTI_REPOS_DIR`     | `--repos-dir`     | Colon-separated directories containing repositories              |
| `SHANTI_WORKTREES_DIR` | `--worktrees-dir` | Directory where spaces are created                               |
| `SHANTI_RUN_FETCH`     | `--run-fetch`     | Fetch every repository at startup                                |
| `SHANTI_THEME`         | `--theme`         | Colour scheme to use, e.g. `catppuccin-latte`                    |
| `SHANTI_CONFIG`        | `--config`        | Directory holding `config.toml` (the flag names the file itself) |
| `SHANTI_JJ_BIN`        | —                 | Path to the `jj` binary when it is not on `PATH`                 |
| `SHANTI_DATA`          | —                 | Directory for shanti's log file and `spaces.toml`                |
| `SHANTI_LOGLEVEL`      | —                 | Log level, e.g. `debug` (`RUST_LOG` takes precedence)            |
| `SHANTI_NO_HOOKS`      | `--no-hooks`      | Skip every post-create hook for this run (any non-empty value)   |
| `GITHUB_TOKEN`         | —                 | Read-only token for the GitHub PR flow                           |

The file keys `backend` and `editor` are parsed and reported but nothing acts on
them yet: the backend is decided from the repository on disk. The `theme` key,
the `[hooks]` and the `[repos.<name>.hooks]` tables _do_ act — `theme` names a
scheme from `theme::scheme::ALL` and is installed once at startup, and the hooks
are described under **Post-create hooks** above.

## Conventions

- Use `color-eyre` for error propagation: `eyre::Result`, `.wrap_err("...")`.
- Use `tracing` macros (`debug!`, `error!`) for logging — no `println!` in library
  code. `main.rs` is the only place that writes to stdout (the selected path,
  which `cd $(shanti)` consumes) or stderr.
- Comment the _why_ — intent, trade-offs, gotchas — not the _what_. The modules
  in `src/vcs/` are the house style: a module-level doc comment stating the rule
  the module exists to enforce.
- A failed fetch costs a stale view of the remotes and nothing else: never drop a
  repository from the list or abort a flow because of it (`vcs::refresh`).
- git fetch authenticates through the **SSH agent** only
  (`Cred::ssh_key_from_agent` in `vcs/git/backend.rs`); HTTPS credentials are not
  implemented. jj fetches through `jj git fetch`, so it uses whatever jj is
  configured with.
- New popups implement the `Modal` trait (`area`, `draw`, `handle`, optionally
  `mode` and `help`) rather than adding state to `App`.
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

> **`CLAUDE.md` and `AGENTS.md` are independent files with the same content.**
> Any change to one must be mirrored in the other, except the generated Beads
> blocks below.
>
> The H1 title necessarily differs between the two, so this divergence is
> deliberate: <!-- bd-doctor-divergence: ok -->

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
