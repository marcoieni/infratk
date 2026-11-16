use std::process::ExitCode;

use crate::{command::current_branch, config::Config, grouped_dirs::GroupedDirs};

pub fn apply(config: &Config) -> ExitCode {
    let affected_modules = current_branch::affected_modules("apply");
    if affected_modules.is_empty() {
        return ExitCode::SUCCESS;
    }
    println!("\nTerraform/Terragrunt will show a plan and ask for confirmation for each module.");

    let failed_directories = GroupedDirs::new(&affected_modules).apply_all(config);
    if failed_directories.is_empty() {
        return ExitCode::SUCCESS;
    }

    println!("\nApply didn't succeed in:");
    for directory in &failed_directories {
        println!("  {directory}");
    }
    // Fail even if the user chose to continue, so that scripts running
    // `infratk apply` don't carry on as if every module was applied.
    ExitCode::FAILURE
}
