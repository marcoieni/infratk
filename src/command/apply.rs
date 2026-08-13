use crate::{
    config::Config, dir::current_dir_is_simpleinfra, envirnoment::assert_aws_env_is_not_set, git,
    graph::ModulesGraph, grouped_dirs::GroupedDirs,
};

pub fn apply(config: &Config) {
    let repo = git::repo();
    let git_root = git::git_root(&repo);
    std::env::set_current_dir(&git_root).expect("failed to switch to the repository root");

    assert!(
        current_dir_is_simpleinfra(),
        "infratk apply must be run from the simpleinfra repository"
    );
    assert_aws_env_is_not_set();

    let changed_files = git::current_branch_changed_files(&repo);
    if changed_files.is_empty() {
        println!("No changes found on the current branch.");
        return;
    }

    println!("Files changed on the current branch:");
    for file in &changed_files {
        println!("  {file}");
    }

    let graph = ModulesGraph::new(None);
    let affected_modules = graph.get_affected_modules_containing_lockfile(&changed_files);
    if affected_modules.is_empty() {
        println!("No Terraform or Terragrunt root modules are affected.");
        return;
    }

    println!("\nAffected root modules:");
    for module in &affected_modules {
        println!("  {module}");
    }
    println!("\nTerraform/Terragrunt will show a plan and ask for confirmation for each module.");

    GroupedDirs::new(&affected_modules).apply_all(config);
}
