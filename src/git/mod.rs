mod repository;
mod worktree;

// `list_repositories_excluding` has no caller yet: wiring it into `app.rs` (so the
// worktrees dir is excluded and nested worktrees stop being rediscovered as
// repositories) is tracked by shanti-gmf.9. Drop this attribute with that change.
#[allow(unused_imports)]
pub use repository::{
    list_repositories, list_repositories_excluding, worktrees_of_repositories, Repository,
};
pub use worktree::{delete_worktree, RemoteStatus, Worktree};
