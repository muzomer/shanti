---
name: demos
description: Use when generating or regenerating shanti's README demo GIFs, adding a new demo for a feature, or troubleshooting the demo pipeline. Trigger when the user asks to record, update, or add a demo/GIF, mentions vhs or .tape files, or wants the README's demo media refreshed after a UI change.
---

# Demos

Shanti's README demo GIFs are produced with [vhs](https://github.com/charmbracelet/vhs) driving a real `shanti` binary against fixture repos, no Docker involved. Two things make this reliable enough for an agent to run unattended:

1. **The binary is built locally.** `cargo build --release`, then put `target/release` on `PATH` before invoking `vhs`.
2. **Fixtures are local, not GitHub-hosted**, wherever shanti's feature under demo doesn't require GitHub. This makes most demos reproducible offline — no SSH, no dependency on repos under a specific account staying in a specific state.

## Two fixture kinds

| tape | fixtures | network? |
| --- | --- | --- |
| `demos/features.tape` | `demos/fixtures/setup-local.sh` — generates git repos (`frontend-repo`, `backend-repo`, each with a real bare "origin" so status glyphs have something to show) and one jj-colocated repo (`infra-repo`) under `demos/repos-dir/` | no |
| `demos/create-from-pr.tape` | `demos/fixtures/setup-github.sh` — clones the real `muzomer/first-repo` / `muzomer/second-repo` fixtures over SSH | yes |

The split exists because "create a space from a PR link" (`src/github.rs`) calls the real GitHub API (or `gh`) and does `git clone git@github.com:<owner>/<repo>.git` — there's no local stand-in for that short of standing up a fake GitHub API, which isn't worth it for a demo. Every other feature (create/navigate/filter/delete/select a space, both backends, colour schemes, detail pane, help popup) needs nothing beyond a local git/jj repo, so it uses `setup-local.sh`.

**Default to the local, offline path.** Only reach for `setup-github.sh` when the thing being demoed is specifically the PR-link flow.

## Regenerating an existing demo

```bash
task build-shanti
task setup-demo-fixtures        # or setup-demo-fixtures-github for create-from-pr.tape
cd demos
PATH="$(git rev-parse --show-toplevel)/target/release:$PATH" vhs features.tape
```

(`task create-demo` / `task create-pr-demo` do all of the above in one shot — see `Taskfile.yml`.)

The output path is whatever `Output` says at the top of the `.tape` file, and lands directly in `demos/`. Check the resulting GIF before treating the task as done — vhs can run clean and still produce a demo that doesn't match the current UI (a keybinding changed, a screen's copy changed, timing races a slow render).

## Adding a new demo

1. Decide if it needs GitHub. If not, write against `demos/repos-dir/` as populated by `setup-local.sh` — don't invent new fixture repos in the tape itself; extend `setup-local.sh` if the demo needs a repo shape that doesn't exist yet (e.g. a repo with local uncommitted changes, to show jj's auto-commit behavior).
2. Copy the header block from `demos/features.tape` (`FontSize 20`, `Theme TokyoNight`, `Padding 2`, `BorderRadius 8`, `TypingSpeed 75ms`) so every demo shares one look. Don't invent a new theme or font size per-demo.
3. Hide the `export SHANTI_REPOS_DIR=...` / `SHANTI_WORKTREES_DIR=...` preamble (`Hide` / `Show`) — viewers don't need to see env setup.
4. Keep one tape scoped to one story. Prefer a new small `.tape` file over growing `features.tape` further — it's already the README hero and doesn't need to carry every feature.
5. Run it, watch the GIF, iterate on `Sleep` durations — vhs doesn't wait for shanti to finish rendering a frame, so an action immediately followed by too short a `Sleep` gets cut off in the recording even though the real app rendered it fine.

## Rules

- Never commit `demos/repos-dir/`, `demos/worktrees-dir/`, or `demos/.remotes/` — all gitignored, regenerate don't hand-edit.
- Don't add Docker back. The old pipeline built shanti and ran vhs inside a container to get a consistent environment; running `cargo build --release` + the system `vhs` binary directly is simpler and was the whole point of replacing it.
- If `jj` isn't on `PATH`, `setup-local.sh` skips the `infra-repo` fixture rather than failing — mirrors how `src/vcs/jj/testing.rs` skips jj tests when `jj` is missing. Don't make a demo hard-depend on the jj fixture existing without checking for this.
