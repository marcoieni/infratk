use camino::Utf8PathBuf;

use crate::{
    dir::current_dir_is_simpleinfra, envirnoment::assert_aws_env_is_not_set, git,
    graph::ModulesGraph,
};

/// Switch to the repository root and return the Terraform/Terragrunt root
/// modules affected by the current branch's changes, printing the changed
/// files and the affected modules.
///
/// Returns an empty list, after telling the user why, if there's nothing to do.
pub fn affected_modules(command_name: &str) -> Vec<Utf8PathBuf> {
    let repo = git::repo();
    let git_root = git::git_root(&repo);
    std::env::set_current_dir(&git_root).expect("failed to switch to the repository root");

    assert!(
        current_dir_is_simpleinfra(),
        "infratk {command_name} must be run from the simpleinfra repository"
    );
    assert_aws_env_is_not_set();

    let changed_files = git::current_branch_changed_files(&repo);
    if changed_files.is_empty() {
        println!("No changes found on the current branch.");
        return vec![];
    }

    println!("Files changed on the current branch:");
    for file in &changed_files {
        println!("  {file}");
    }

    let graph = ModulesGraph::new(None);
    let affected_modules = graph.get_affected_modules_containing_lockfile(&changed_files);
    if affected_modules.is_empty() {
        println!("No Terraform or Terragrunt root modules are affected.");
    } else {
        println!("\nAffected root modules:");
        for module in &affected_modules {
            println!("  {module}");
        }
    }
    affected_modules
}
