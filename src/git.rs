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
    if let Ok(symbolic_ref) = repo.git(&[
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ]) {
        return symbolic_ref.trim().to_string();
    }

    for candidate in ["origin/master", "origin/main", "master", "main"] {
        if repo
            .git(&["rev-parse", "--verify", "--quiet", candidate])
            .is_ok()
        {
            return candidate.to_string();
        }
    }

    panic!("could not determine the repository's default branch");
}
