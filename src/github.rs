use color_eyre::eyre::{self, WrapErr};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tracing::debug;

#[derive(Clone)]
pub struct PrUrl {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

#[derive(Clone)]
pub struct PrInfo {
    pub branch_name: String,
    pub is_merged: bool,
}

impl PrUrl {
    /// The canonical URL this was parsed from.
    ///
    /// Rebuilt rather than kept, so what is remembered against a space is one
    /// canonical form — a URL pasted with a `#discussion` fragment or a trailing
    /// slash files under the same key as the plain one.
    pub fn to_url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.repo, self.number
        )
    }
}

/// Parses a GitHub PR URL of the form:
/// `https://github.com/{owner}/{repo}/pull/{number}`
pub fn parse_pr_url(url: &str) -> eyre::Result<PrUrl> {
    let path = url
        .strip_prefix("https://github.com/")
        .ok_or_else(|| eyre::eyre!("Not a GitHub URL — must start with https://github.com/"))?;

    let parts: Vec<&str> = path.splitn(4, '/').collect();
    if parts.len() < 4 || parts[2] != "pull" {
        eyre::bail!("Invalid GitHub PR URL — expected: https://github.com/owner/repo/pull/NUMBER");
    }

    let number: u64 = parts[3]
        .parse()
        .wrap_err("PR number must be a positive integer")?;

    Ok(PrUrl {
        owner: parts[0].to_string(),
        repo: parts[1].to_string(),
        number,
    })
}

/// How the PR flow obtains PR data.
///
/// The flow is injected with this rather than calling [`fetch_pr_info`] directly
/// because everything after the fetch — the "clone this repo?" prompt and the
/// repos-dir picker — is unreachable until a fetch succeeds. Handing the lookup
/// in keeps those steps drivable without a network round trip, and leaves room
/// for a cached or background fetcher later.
pub type PrFetcher = Arc<dyn Fn(&PrUrl) -> eyre::Result<PrInfo> + Send + Sync>;

/// The fetcher used by the real application: a live GitHub lookup.
pub fn live_fetcher() -> PrFetcher {
    Arc::new(fetch_pr_info)
}

/// Fetches PR info. Authentication priority:
/// 1. `gh api` — uses `GITHUB_TOKEN` env var if set (fine-grained PAT), otherwise `gh` stored credentials
/// 2. `ureq` with `GITHUB_TOKEN` — pure-Rust fallback when `gh` CLI is not installed
pub fn fetch_pr_info(pr: &PrUrl) -> eyre::Result<PrInfo> {
    let endpoint = format!("/repos/{}/{}/pulls/{}", pr.owner, pr.repo, pr.number);

    match Command::new("gh").args(["api", &endpoint]).output() {
        Ok(output) if output.status.success() => {
            return parse_pr_json(&output.stdout);
        }
        Ok(output) => {
            // gh is installed but the request failed
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            if stderr.contains("authentication")
                || stderr.contains("not logged")
                || stderr.is_empty()
            {
                eyre::bail!("GitHub auth failed — set GITHUB_TOKEN or run `gh auth login`");
            }
            eyre::bail!("GitHub API error: {}", stderr);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // gh not installed — fall through to ureq fallback
        }
        Err(e) => eyre::bail!("Failed to run gh: {}", e),
    }

    // gh not available: try GITHUB_TOKEN with ureq
    let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
        eyre::eyre!(
            "GitHub CLI (gh) not found and GITHUB_TOKEN not set\n\
             Install gh: https://cli.github.com  or  set GITHUB_TOKEN"
        )
    })?;

    fetch_via_ureq(pr, &token)
}

fn fetch_via_ureq(pr: &PrUrl, token: &str) -> eyre::Result<PrInfo> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pulls/{}",
        pr.owner, pr.repo, pr.number
    );

    let response = ureq::get(&url)
        .header("Authorization", &format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "shanti")
        .call()
        .wrap_err("GitHub API request failed")?;

    let bytes = response
        .into_body()
        .read_to_vec()
        .wrap_err("Failed to read GitHub API response")?;

    parse_pr_json(&bytes)
}

/// Clones a GitHub repository into `<repos_dir>/<repo>` using SSH.
///
/// # Why plain `git clone` and not `jj git clone --colocate`
///
/// The PR flow can reach a repository that is not on disk yet, and the clone has
/// to pick a backend for it before anyone has said which one they want. shanti
/// clones with **git**, deliberately, and leaves jj to the user:
///
/// * **It works on every machine.** `jj git clone` needs a jj that is installed
///   and new enough ([`MINIMUM_JJ_VERSION`](crate::vcs::jj::MINIMUM_JJ_VERSION)).
///   Cloning with jj would make opening a pull request fail for the majority of
///   users, who have no jj at all, for no benefit to them.
/// * **It imposes nothing.** A clone is the moment a repository's shape is
///   decided, and deciding it *for* someone who never chose jj is not shanti's
///   call to make. git is the shape GitHub itself hands out.
/// * **Adopting jj afterwards is one command and costs nothing.** Since
///   shanti-nhe.9 a colocated repository is driven fully through jj, so a user
///   who wants it runs `jj git init --colocate` in the clone and shanti picks it
///   up on the next scan — no re-clone, no lost work, no configuration here.
///
/// The reverse choice has no such escape hatch: a jj clone on a machine whose
/// owner does not use jj is a repository they cannot get rid of without
/// deleting `.jj` by hand.
///
/// Nothing downstream hard-codes the outcome. [`crate::vcs::open_at`] decides
/// the backend from what is on disk, so if this ever becomes a user preference,
/// this function is the only place that changes.
///
/// # The destination must not exist
///
/// A clone cannot be interrupted, only abandoned, so a half-written destination
/// is a thing shanti has to expect to meet — left by an abandoned clone whose
/// tidy-up did not finish, or by quitting while one was running. This function
/// therefore **refuses** rather than cloning on top of anything already at the
/// destination, and names the path so the user can look at it and decide: an
/// interrupted clone is often a working repository that `git fetch` completes,
/// and it is not shanti's place to delete a directory it did not create. The
/// same refusal covers a directory the user put there for their own reasons.
///
/// The directory this call *does* create is removed again by [`discard_clone`]
/// when the clone fails or when nobody wants the result any more, so the common
/// case never reaches the refusal at all.
pub fn clone_repository(owner: &str, repo: &str, repos_dir: &str) -> eyre::Result<PathBuf> {
    // The name goes straight into a path, and it arrives from a URL the user
    // pasted. One segment, nothing that can climb: this is what makes the
    // destination below a path shanti chose rather than one the input chose.
    let repo = one_path_segment(repo).wrap_err("Refusing to clone into an unsafe path")?;
    let owner = one_path_segment(owner).wrap_err("Refusing to clone from an unsafe name")?;

    let dest = Path::new(repos_dir).join(repo);

    // Claim the destination by *creating* it, rather than by testing whether it
    // exists and cloning afterwards. `create_dir` fails if anything is already
    // there, so the claim is atomic: past this line the directory is one this
    // call brought into being, which is the whole licence for removing it again
    // in `discard_clone` and in the failure arm below.
    match fs::create_dir(&dest) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            eyre::bail!(
                "{} already exists — an earlier clone may have been interrupted. \
                 Remove it, or open it and finish it with `git fetch`, then rescan",
                dest.display()
            );
        }
        Err(e) => {
            return Err(e).wrap_err(format!("Could not create {}", dest.display()));
        }
    }

    let url = format!("git@github.com:{}/{}.git", owner, repo);
    // Cloning into the directory just claimed: git accepts an existing *empty*
    // destination, which is exactly what the claim leaves behind.
    let output = Command::new("git")
        .arg("clone")
        .arg(&url)
        .arg(&dest)
        .output();

    let output = match output {
        Ok(output) => output,
        Err(e) => {
            // Nothing was cloned, so the empty claim must not outlive the
            // attempt — otherwise the next try refuses on our own leftover.
            discard_clone(&dest);
            return Err(e).wrap_err("Failed to run git clone");
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim().to_string();
        discard_clone(&dest);
        eyre::bail!("git clone failed: {}", stderr);
    }

    Ok(dest)
}

/// Removes a clone destination that nobody is going to use.
///
/// Only ever called with a path [`clone_repository`] created — either because
/// its own `git clone` failed, or because the caller threw the result away. It
/// is best effort by design: failing to tidy up is not worth turning into a
/// second error on top of the one that got us here, and the leftover is caught
/// by the refusal above on the next attempt anyway.
pub fn discard_clone(dest: &Path) {
    // The destination was a directory when we made it. If it is not one now,
    // something else has been at this path and it is no longer ours to delete.
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            debug!(path = %dest.display(), "not removing an abandoned clone: no longer a directory");
            return;
        }
        Err(_) => return,
    }

    if let Err(e) = fs::remove_dir_all(dest) {
        debug!(path = %dest.display(), error = %e, "could not remove an abandoned clone");
    }
}

/// Accepts a name that is exactly one path component, and nothing else.
///
/// `.`, `..`, anything with a separator, and the empty string are rejected, so
/// no name out of a pasted URL can point the destination somewhere other than
/// directly inside the repos dir.
fn one_path_segment(name: &str) -> eyre::Result<&str> {
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(part)), None) if part == name => Ok(name),
        _ => eyre::bail!("{name:?} is not a plain repository name"),
    }
}

fn parse_pr_json(bytes: &[u8]) -> eyre::Result<PrInfo> {
    let json: serde_json::Value =
        serde_json::from_slice(bytes).wrap_err("Failed to parse GitHub API response")?;

    // GitHub returns {"message": "..."} on errors (e.g. 404, bad token)
    if let Some(msg) = json["message"].as_str() {
        eyre::bail!("GitHub API error: {}", msg);
    }

    let branch_name = json["head"]["ref"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("Unexpected GitHub API response: missing head.ref"))?
        .to_string();

    let is_merged = json["merged"].as_bool().unwrap_or(false);

    Ok(PrInfo {
        branch_name,
        is_merged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pr_url_valid() {
        let pr =
            parse_pr_url("https://github.com/Pix4D/platform-cloud-django-infra/pull/59").unwrap();
        assert_eq!(pr.owner, "Pix4D");
        assert_eq!(pr.repo, "platform-cloud-django-infra");
        assert_eq!(pr.number, 59);
    }

    #[test]
    fn test_parse_pr_url_not_github() {
        assert!(parse_pr_url("https://gitlab.com/owner/repo/merge_requests/1").is_err());
    }

    #[test]
    fn test_parse_pr_url_missing_pull_segment() {
        assert!(parse_pr_url("https://github.com/owner/repo/issues/1").is_err());
    }

    #[test]
    fn test_parse_pr_url_non_numeric_number() {
        assert!(parse_pr_url("https://github.com/owner/repo/pull/abc").is_err());
    }

    /// The leftover case, from the other side: whatever is already at the
    /// destination — a half-written clone, or a directory of the user's own —
    /// is reported by name and never written into or deleted.
    #[test]
    fn cloning_refuses_when_the_destination_already_exists() {
        let repos = tempfile::tempdir().expect("a temp repos dir");
        let dest = repos.path().join("shanti");
        fs::create_dir(&dest).expect("the leftover");
        fs::write(dest.join("half-written"), b"x").expect("its contents");

        let error = clone_repository("owner", "shanti", repos.path().to_str().unwrap())
            .expect_err("cloning onto a leftover");

        let message = format!("{error:#}");
        assert!(
            message.contains(&dest.display().to_string()),
            "the message must name the directory: {message}"
        );
        assert!(
            dest.join("half-written").exists(),
            "refusing must not touch what is already there"
        );
    }

    /// No name out of a pasted URL may steer the destination out of the repos
    /// dir — checked before anything is created and before git is run.
    #[test]
    fn cloning_rejects_a_name_that_is_not_one_path_segment() {
        let repos = tempfile::tempdir().expect("a temp repos dir");
        let dir = repos.path().to_str().unwrap();

        for name in ["..", ".", "", "a/b", "../elsewhere"] {
            assert!(
                clone_repository("owner", name, dir).is_err(),
                "{name:?} was accepted as a repository name"
            );
        }
        assert!(clone_repository("../owner", "shanti", dir).is_err());
        assert!(
            !repos.path().join("shanti").exists(),
            "a rejected name must not have claimed a directory"
        );
    }

    /// The tidy-up only ever removes a directory. Anything else at the path is
    /// no longer the thing this call created, so it is left alone.
    #[test]
    fn discarding_leaves_anything_that_is_not_a_directory() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let file = dir.path().join("not-a-clone");
        fs::write(&file, b"x").expect("the file");

        discard_clone(&file);

        assert!(file.exists(), "a file was removed as if it were a clone");
    }
}
