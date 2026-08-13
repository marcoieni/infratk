use camino::Utf8PathBuf;
use git_cmd::Repo;

use crate::{cmd::Cmd, dir};

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
    git_cmd::Repo::new(current_dir).unwrap()
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
    let remote = upstream_remote(repo).unwrap_or_else(|| "origin".to_string());
    let remote_head = format!("refs/remotes/{remote}/HEAD");
    if let Ok(symbolic_ref) = repo.git(&["symbolic-ref", "--quiet", "--short", &remote_head]) {
        return symbolic_ref.trim().to_string();
    }

    let remote_main = format!("{remote}/main");
    let remote_master = format!("{remote}/master");
    for candidate in [
        remote_main.as_str(),
        remote_master.as_str(),
        "main",
        "master",
    ] {
        if repo
            .git(&["rev-parse", "--verify", "--quiet", candidate])
            .is_ok()
        {
            return candidate.to_string();
        }
    }

    panic!("could not determine the repository's default branch");
}

fn upstream_remote(repo: &Repo) -> Option<String> {
    let upstream = repo
        .git(&[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .ok()?;
    let (remote, _) = upstream.trim().split_once('/')?;
    (remote != ".").then(|| remote.to_string())
}

#[cfg(test)]
mod tests {
    use std::process::Command as ProcessCommand;

    use camino::Utf8Path;
    use camino_tempfile::Utf8TempDir;

    use super::*;

    #[test]
    fn default_branch_uses_current_branch_upstream_remote() {
        let repository = test_repo_with_upstream("trunk", true);
        let repo = Repo::new(repository.path()).unwrap();

        assert_eq!(default_branch_ref(&repo), "upstream/trunk");
    }

    #[test]
    fn default_branch_falls_back_with_missing_remote_head() {
        let repository = test_repo_with_upstream("main", false);
        let repo = Repo::new(repository.path()).unwrap();

        assert_eq!(default_branch_ref(&repo), "upstream/main");
    }

    fn test_repo_with_upstream(default_branch: &str, set_remote_head: bool) -> Utf8TempDir {
        let repository = Utf8TempDir::new().unwrap();
        run_git(repository.path(), &["init", "--initial-branch=feature"]);
        run_git(repository.path(), &["config", "user.name", "Test User"]);
        run_git(
            repository.path(),
            &["config", "user.email", "test@example.com"],
        );
        fs_err::write(repository.path().join("README.md"), "test").unwrap();
        run_git(repository.path(), &["add", "README.md"]);
        run_git(repository.path(), &["commit", "-m", "initial"]);
        run_git(
            repository.path(),
            &[
                "remote",
                "add",
                "upstream",
                "https://example.invalid/repo.git",
            ],
        );
        run_git(
            repository.path(),
            &["update-ref", "refs/remotes/upstream/feature", "HEAD"],
        );
        run_git(
            repository.path(),
            &[
                "update-ref",
                &format!("refs/remotes/upstream/{default_branch}"),
                "HEAD",
            ],
        );
        if set_remote_head {
            run_git(
                repository.path(),
                &[
                    "symbolic-ref",
                    "refs/remotes/upstream/HEAD",
                    &format!("refs/remotes/upstream/{default_branch}"),
                ],
            );
        }
        run_git(
            repository.path(),
            &["branch", "--set-upstream-to=upstream/feature", "feature"],
        );
        repository
    }

    fn run_git(repository: &Utf8Path, args: &[&str]) {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
