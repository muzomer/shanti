use color_eyre::eyre::{self, WrapErr};
use std::io;
use std::process::Command;
use std::sync::Arc;

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
pub fn clone_repository(owner: &str, repo: &str, repos_dir: &str) -> eyre::Result<()> {
    let url = format!("git@github.com:{}/{}.git", owner, repo);
    let dest = format!("{}/{}", repos_dir, repo);
    let output = Command::new("git")
        .args(["clone", &url, &dest])
        .output()
        .wrap_err("Failed to run git clone")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eyre::bail!("git clone failed: {}", stderr.trim());
    }
    Ok(())
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
}
