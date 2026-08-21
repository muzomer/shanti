//! The jujutsu backend's plumbing: one adapter, one process boundary.
//!
//! shanti drives jj through its command-line tool rather than by linking
//! `jj_lib`. The CLI plus explicit templates is jj's documented, stable
//! contract; `jj_lib` offers no stability guarantee across releases, would pull
//! a very large dependency tree into a small TUI, and would pin users to
//! whichever jj version shanti happened to be compiled against. Shelling out
//! lets a user upgrade jj without rebuilding shanti.
//!
//! Everything jj-related therefore funnels through [`JjCli`]: it is the only
//! place in the codebase that spawns a process, so the guarantees it makes
//! (no pager, no colour, an explicit repository, a checked version, machine-
//! readable output) hold everywhere by construction instead of by convention.
//!
//! This module is plumbing only — it does not implement
//! [`Vcs`](crate::vcs::Vcs). Mapping jj workspaces onto
//! [`Space`](crate::vcs::Space) is the next layer up and lives elsewhere.

mod cmd;
mod template;
mod version;

pub use cmd::{JjCli, WorkingCopy, JJ_BINARY_ENV};
pub use template::{Record, Template, FIELD_SEPARATOR, RECORD_SEPARATOR};
pub use version::{JjVersion, MINIMUM_JJ_VERSION};
