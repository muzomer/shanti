//! The repositories shanti has open, and the questions callers ask about them.
//!
//! Kept apart from the widget that lists them, because they are two different
//! jobs: this owns the open backends and answers questions about them, while a
//! list widget owns a cursor, a filter and a focus. Fold them together and
//! anything wanting a domain answer — which backend owns this space? what is
//! this repository called? — has to reach through a widget to get it, and every
//! new surface takes a dependency on the list.
//!
//! So the store knows nothing about selection. A widget owns one and adds the
//! cursor on top; anything else borrows one and just asks.
//!
//! # Colocated repositories are open twice
//!
//! Almost every subtlety in this module comes from one fact: a colocated
//! repository is opened once per backend, and both copies share a [`RepoId`],
//! because the id is derived from the path. So:
//!
//! * [`RepoStore::backend_for`] matches on the id **and** the backend, or a git
//!   worktree's deletion would be handed to jj, which knows nothing about it;
//! * [`RepoStore::remove`] drops *every* backend on an id, or half a repository
//!   would be left behind;
//! * [`RepoStore::paths`] deduplicates, or such a repository would be re-read
//!   twice to produce the same rows;
//! * [`RepoStore::choices`] keeps one entry per repository, preferring jj —
//!   which is what makes a new space on a colocated repository a jj workspace
//!   (see `docs/adr/0002-jj-owns-a-colocated-repository.md`).

use std::collections::HashSet;
use std::path::PathBuf;

use super::{Backend, BoxedVcs, Repo, RepoId, Space, Vcs};

/// Every repository shanti has open, each behind the backend that drives it.
///
/// Holding [`BoxedVcs`] rather than a concrete backend is what makes the
/// collection heterogeneous: git and jj repositories sit side by side and
/// nothing above this type asks which is which.
#[derive(Default)]
pub struct RepoStore {
    backends: Vec<BoxedVcs>,
}

impl RepoStore {
    pub fn new(backends: Vec<BoxedVcs>) -> Self {
        Self { backends }
    }

    // --- Mutation ------------------------------------------------------------
    //
    // None of these touch a selection: a caller that has a cursor is responsible
    // for anchoring it before and restoring it after. Keeping that out of here
    // is the entire point of the split.

    /// Add one backend, without checking whether its repository is already held.
    pub fn add(&mut self, backend: BoxedVcs) {
        self.backends.push(backend);
    }

    /// Swap the whole set.
    ///
    /// What a scan result needs and [`RepoStore::merge`] cannot do: the set is
    /// *replaced*, so a repository that has since left the disk does not survive
    /// the rescan that failed to find it.
    pub fn replace(&mut self, backends: Vec<BoxedVcs>) {
        self.backends = backends;
    }

    /// Drop every backend open on `id`, and say how many that was.
    pub fn remove(&mut self, id: &RepoId) -> usize {
        let before = self.backends.len();
        self.backends.retain(|backend| &backend.repo().id != id);
        before - self.backends.len()
    }

    /// Merge a batch of freshly opened backends in.
    ///
    /// A repository already held is replaced rather than added beside itself, so
    /// overlapping repos dirs — or a second scan — cannot show the same
    /// repository twice.
    pub fn merge(&mut self, backends: Vec<BoxedVcs>) {
        let arriving: HashSet<RepoId> = backends
            .iter()
            .map(|backend| backend.repo().id.clone())
            .collect();
        for id in &arriving {
            self.remove(id);
        }
        self.backends.extend(backends);
    }

    // --- Queries -------------------------------------------------------------

    /// The backend that owns `space`, if it is still open.
    ///
    /// A [`Space`] is an inert snapshot; every action on one — deleting it above
    /// all — has to go back through the repository it came from, and this store
    /// is the only place those live.
    pub fn backend_for(&self, space: &Space) -> Option<&dyn Vcs> {
        self.backends
            .iter()
            .find(|backend| backend.repo().id == space.repo && backend.backend() == space.backend)
            .map(|backend| backend.as_ref())
    }

    /// The repository `id` driven by `backend`, falling back to whatever backend
    /// owns it.
    ///
    /// The fallback is what lets a single-backend repository always resolve: the
    /// create prompt offers a choice only on a colocated repository, and asks
    /// for one everywhere.
    pub fn backend_of(&self, id: &RepoId, backend: Backend) -> Option<&dyn Vcs> {
        self.backends
            .iter()
            .find(|b| &b.repo().id == id && b.backend() == backend)
            .or_else(|| self.backends.iter().find(|b| &b.repo().id == id))
            .map(|b| b.as_ref())
    }

    /// The snapshot of the repository `id`, from whichever backend owns it.
    ///
    /// The name and root a row is labelled with, recovered from an id alone —
    /// which is all a background result carries back. Any backend will do: both
    /// copies of a colocated repository describe the same directory.
    pub fn repository(&self, id: &RepoId) -> Option<&Repo> {
        self.backends
            .iter()
            .map(|backend| backend.repo())
            .find(|repo| &repo.id == id)
    }

    /// Every repository held, once each, as something a job can be given.
    pub fn paths(&self) -> Vec<PathBuf> {
        let mut seen: HashSet<&RepoId> = HashSet::new();
        self.backends
            .iter()
            .map(|backend| backend.repo())
            .filter(|repo| seen.insert(&repo.id))
            .map(|repo| repo.path.clone())
            .collect()
    }

    /// Every backend open on the repository `id`, in the order they were opened
    /// — the owner first.
    ///
    /// More than one means a colocated repository, which is the only case the UI
    /// has to explain: "new space" there is ambiguous.
    pub fn backends_of(&self, id: &RepoId) -> Vec<Backend> {
        self.backends
            .iter()
            .filter(|backend| &backend.repo().id == id)
            .map(|backend| backend.backend())
            .collect()
    }

    /// One entry per repository, for a list that asks "which repository?" rather
    /// than "which backend?".
    ///
    /// The entry kept for a colocated repository is the jj one — see the module
    /// note.
    pub fn choices(&self) -> Vec<&BoxedVcs> {
        let mut chosen: Vec<&BoxedVcs> = Vec::with_capacity(self.backends.len());
        for candidate in &self.backends {
            match chosen
                .iter_mut()
                .find(|kept| kept.repo().id == candidate.repo().id)
            {
                Some(kept) => {
                    if candidate.backend() == Backend::Jj {
                        *kept = candidate;
                    }
                }
                None => chosen.push(candidate),
            }
        }
        chosen
    }

    /// Every backend held, in the order they were opened.
    pub fn backends(&self) -> &[BoxedVcs] {
        &self.backends
    }

    pub fn len(&self) -> usize {
        self.backends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::SpaceStatus;
    use color_eyre::eyre;
    use std::path::Path;

    /// A backend that answers about one repository and nothing else.
    ///
    /// What matters here is not version control but *identity*: two backends can
    /// share one repository — which is what a colocated repo is — and still be
    /// told apart. A stub also keeps these tests free of a `jj` binary, which
    /// the machine running them may not have.
    struct StubVcs {
        repo: Repo,
    }

    impl StubVcs {
        fn boxed(path: &str, backend: Backend) -> BoxedVcs {
            let name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            Box::new(StubVcs {
                repo: Repo::new(name, path, backend),
            })
        }
    }

    impl Vcs for StubVcs {
        fn repo(&self) -> &Repo {
            &self.repo
        }
        fn spaces(&self) -> eyre::Result<Vec<Space>> {
            Ok(Vec::new())
        }
        fn create_space(&self, _: &str, _: &Path) -> eyre::Result<Space> {
            eyre::bail!("stub")
        }
        fn delete_space(&self, _: &Space) -> eyre::Result<()> {
            Ok(())
        }
        fn fetch(&self) -> eyre::Result<()> {
            Ok(())
        }
        fn resolve_base(&self, _: &str) -> String {
            String::new()
        }
    }

    /// The one repository open twice, which is where every subtlety here comes
    /// from.
    fn colocated() -> RepoStore {
        RepoStore::new(vec![
            StubVcs::boxed("/repos/shanti", Backend::Jj),
            StubVcs::boxed("/repos/shanti", Backend::Git),
        ])
    }

    /// Removing a colocated repository must take *both* of its backends:
    /// leaving one behind leaves a picker offering half a repository.
    #[test]
    fn removing_a_repository_drops_every_backend_it_was_open_as() {
        let mut store = colocated();
        assert_eq!(store.remove(&RepoId::from_path("/repos/shanti")), 2);
        assert!(store.is_empty());
        assert_eq!(
            store.remove(&RepoId::from_path("/repos/shanti")),
            0,
            "removing it again takes nothing"
        );
    }

    /// A space is routed by the id *and* the backend. Matching on the id alone
    /// would hand a git worktree's deletion to jj, which knows nothing about it.
    #[test]
    fn a_space_is_routed_to_the_backend_that_owns_it() {
        let store = colocated();
        let id = RepoId::from_path("/repos/shanti");
        for backend in [Backend::Git, Backend::Jj] {
            let space = Space::new(
                id.clone(),
                backend,
                "feature",
                "/spaces/feature",
                SpaceStatus::unknown(backend),
            );
            let found = store.backend_for(&space).expect("a backend owns it");
            assert_eq!(found.backend(), backend);
        }
    }

    /// A colocated repository has one directory, so re-reading it twice would
    /// cost twice as much to produce the same rows.
    #[test]
    fn paths_are_deduplicated_across_backends() {
        assert_eq!(colocated().paths().len(), 1);
    }

    /// The jj entry is the one kept, which is what makes a new space on a
    /// colocated repository a jj workspace. See ADR-0002.
    #[test]
    fn a_colocated_repository_offers_one_choice_and_it_is_jj() {
        let store = colocated();
        let choices = store.choices();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].backend(), Backend::Jj);
    }

    /// A repository arriving twice — from overlapping repos dirs, or a second
    /// scan — is held once, not beside itself.
    #[test]
    fn merging_replaces_a_repository_rather_than_duplicating_it() {
        let mut store = RepoStore::new(Vec::new());
        for _ in 0..2 {
            store.merge(vec![StubVcs::boxed("/repos/shanti", Backend::Jj)]);
        }
        assert_eq!(store.len(), 1);
    }

    /// The fallback is what lets a single-backend repository always resolve: the
    /// create prompt asks for a backend everywhere, but only offers a choice on
    /// a colocated repo.
    #[test]
    fn asking_for_an_unopened_backend_falls_back_to_the_owner() {
        let store = RepoStore::new(vec![StubVcs::boxed("/repos/shanti", Backend::Jj)]);
        let id = RepoId::from_path("/repos/shanti");
        let found = store.backend_of(&id, Backend::Git).expect("it resolves");
        assert_eq!(found.backend(), Backend::Jj);
    }
}
