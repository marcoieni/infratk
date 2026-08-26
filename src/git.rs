use std::process::Command;

use anyhow::{anyhow, Context as _};
use camino::{Utf8Path, Utf8PathBuf};

use crate::{cmd::Cmd, dir};

#[derive(Debug)]
pub struct Repo {
    directory: Utf8PathBuf,
}

impl Repo {
    fn new(directory: impl AsRef<Utf8Path>) -> anyhow::Result<Self> {
        let directory = directory.as_ref();
        git_in_dir(directory, &["rev-parse", "--verify", "HEAD"])
            .context("cannot initialize git repository")?;

        Ok(Self {
            directory: directory.to_path_buf(),
        })
    }

    pub fn directory(&self) -> &Utf8Path {
        &self.directory
    }

    pub fn git(&self, args: &[&str]) -> anyhow::Result<String> {
        git_in_dir(&self.directory, args)
    }

    pub fn changes_except_typechanges(&self) -> anyhow::Result<Vec<String>> {
        let output = self.git(&["status", "--porcelain"])?;
        Ok(output
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("T "))
            .filter_map(|line| line.rsplit(' ').next())
            .map(str::to_string)
            .collect())
    }
}

/// Return the short status of the repository containing `directory`.
///
/// An empty string means that the working tree and index are clean. Untracked
/// files are included so callers can safely use this before mutating
/// infrastructure.
pub fn working_tree_status(directory: &Utf8Path) -> anyhow::Result<String> {
    Repo::new(directory)?.git(&["status", "--short"])
}

fn git_in_dir(directory: &Utf8Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git in {directory}"))?;
    let stdout = String::from_utf8(output.stdout)
        .context("git produced output that was not valid UTF-8")?
        .trim()
        .to_string();

    if output.status.success() {
        return Ok(stdout);
    }

    let stderr = String::from_utf8(output.stderr)
        .context("git produced an error that was not valid UTF-8")?;
    let mut details = Vec::new();
    if !stdout.is_empty() {
        details.push(format!("stdout: {stdout}"));
    }
    if !stderr.trim().is_empty() {
        details.push(format!("stderr: {}", stderr.trim()));
    }
    let details = if details.is_empty() {
        String::new()
    } else {
        format!(": {}", details.join("; "))
    };
    Err(anyhow!("git {args:?} failed in {directory}{details}"))
}

pub fn assert_current_branch_is_same_as_pr(pr: &str) {
    let current_branch = get_current_branch();
    let pr_branch = get_pr_branch(pr);
    assert_eq!(
        current_branch, pr_branch,
        "You are not in the same branch as the PR locally"
    );
}

fn get_current_branch() -> String {
    Cmd::new("git", ["rev-parse", "--abbrev-ref", "HEAD"])
        .hide_stdout()
        .run()
        .stdout()
        .trim()
        .to_string()
}

fn get_pr_branch(pr: &str) -> String {
    let output = Cmd::new(
        "gh",
        [
            "pr",
            "view",
            pr,
            "--json",
            "headRefName",
            "-q",
            ".headRefName",
        ],
    )
    .hide_stdout()
    .run();
    output.stdout().trim().to_string()
}

pub fn repo() -> Repo {
    let current_dir = dir::current_dir();
    Repo::new(current_dir).unwrap()
}

pub fn git_root(repo: &Repo) -> camino::Utf8PathBuf {
    let output = repo.git(&["rev-parse", "--show-toplevel"]).unwrap();
    output.into()
}

/// Return files changed between the current working tree and the point where
/// the current branch diverged from the repository's default branch.
pub fn current_branch_changed_files(repo: &Repo) -> Vec<Utf8PathBuf> {
    let default_branch = default_branch_ref(repo);
    let merge_base = repo
        .git(&["merge-base", "HEAD", &default_branch])
        .expect("failed to find the merge base with the default branch");
    let merge_base = merge_base.trim();
    let output = repo
        .git(&["diff", "--name-only", merge_base, "--"])
        .expect("failed to list files changed on the current branch");

    output.lines().map(Utf8PathBuf::from).collect()
}

fn default_branch_ref(repo: &Repo) -> String {
    let output = Cmd::new(
        "gh",
        [
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ],
    )
    .with_current_dir(repo.directory())
    .hide_stdout()
    .run();
    assert!(
        output.status().success(),
        "could not determine the repository's default branch"
    );
    output.stdout().to_string()
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;
    use camino_tempfile::Utf8TempDir;

    use super::Repo;

    fn git(directory: &Utf8Path, args: &[&str]) -> String {
        super::git_in_dir(directory, args).unwrap()
    }

    #[test]
    fn repo_accepts_branch_whose_remote_is_a_url() {
        let directory = Utf8TempDir::new().unwrap();
        git(directory.path(), &["init"]);
        git(directory.path(), &["config", "user.name", "Test User"]);
        git(
            directory.path(),
            &["config", "user.email", "test@example.com"],
        );
        git(directory.path(), &["commit", "--allow-empty", "-m", "init"]);
        git(directory.path(), &["branch", "-m", "feature"]);
        git(
            directory.path(),
            &[
                "config",
                "branch.feature.remote",
                "git@github.com:contributor/repository.git",
            ],
        );
        git(
            directory.path(),
            &["config", "branch.feature.merge", "refs/heads/feature"],
        );

        assert!(super::git_in_dir(
            directory.path(),
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )
        .is_err());
        assert!(Repo::new(directory.path()).is_ok());
    }
}
