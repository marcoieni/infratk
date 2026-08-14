use std::collections::BTreeMap;

use camino::Utf8Path;
use secrecy::SecretString;

use crate::cmd::Cmd;

const BACKEND_CONFIGURATION_CHANGED: &str = "Backend configuration changed";

#[derive(Debug, PartialEq)]
pub enum PlanOutcome {
    NoChanges,
    Changes(String),
}

pub struct CmdRunner {
    env_vars: BTreeMap<String, SecretString>,
}

impl CmdRunner {
    pub fn new(env_vars: BTreeMap<String, SecretString>) -> Self {
        Self { env_vars }
    }

    pub fn terragrunt_plan(&self, state: &Utf8Path) -> PlanOutcome {
        self.plan(state, "terragrunt")
    }

    pub fn terraform_plan(&self, module: &Utf8Path) -> PlanOutcome {
        self.plan(module, "terraform")
    }

    pub fn terragrunt_apply(&self, state: &Utf8Path) {
        self.apply(state, "terragrunt", ".terragrunt-cache");
    }

    pub fn terraform_apply(&self, module: &Utf8Path) {
        self.apply(module, "terraform", ".terraform");
    }

    fn apply(&self, directory: &Utf8Path, command: &str, cache_directory_name: &str) {
        let output = Cmd::new(command, ["apply"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .run_interactive_with_output();
        if output.status().success() {
            return;
        }

        assert!(
            backend_configuration_changed(&output),
            "{command} apply failed in {directory}"
        );

        let cache_directory = directory.join(cache_directory_name);
        match fs_err::remove_dir_all(&cache_directory) {
            Ok(()) => println!("Removed stale cache directory {cache_directory}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to remove {cache_directory}: {error}"),
        }

        println!("Backend configuration changed; reinitializing {directory}");
        let init_output = Cmd::new(command, ["init", "-input=false"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .run();
        assert!(
            init_output.status().success(),
            "{command} init failed in {directory}"
        );

        let retry_status = Cmd::new(command, ["apply"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .run_interactive();
        assert!(
            retry_status.success(),
            "{command} apply failed after reinitializing {directory}"
        );
    }

    /// Check if Terragrunt or Terraform plan is clean.
    /// Useful to check wheter there are some unapplied changes in the repo.
    fn plan(&self, directory: &Utf8Path, command: &str) -> PlanOutcome {
        // The `-detailed-exitcode` returns the following exit codes:
        // 0 - Succeeded, diff is empty (no changes)
        // 1 - Errored
        // 2 - Succeeded, there is a diff
        let output = Cmd::new(command, ["plan", "-detailed-exitcode", "-input=false"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .run();
        let is_diff_empty = output.status().code().unwrap() == 0;
        if is_diff_empty {
            PlanOutcome::NoChanges
        } else {
            let plan_details = output.stdout().split("Terraform will perform the following actions:").last().expect("Terraform output did not contain 'Terraform will perform the following actions:'");
            let mut plan_details = match plan_details
                .split(
                    "─────────────────────────────────────────────────────────────────────────────",
                )
                .next()
            {
                Some(plan) => plan,
                None => plan_details,
            }
            .to_string();
            if output.status().code().unwrap() == 1 {
                plan_details.push_str(output.stderr());
            }
            PlanOutcome::Changes(plan_details)
        }
    }

    pub fn terragrunt_init_upgrade(&self, directory: &Utf8Path) {
        self.init_upgrade(directory, "terragrunt");
    }

    pub fn terraform_init_upgrade(&self, directory: &Utf8Path) {
        self.init_upgrade(directory, "terraform");
    }

    fn init_upgrade(&self, directory: &Utf8Path, command: &str) {
        Cmd::new(command, ["init", "--upgrade", "-input=false"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .run();
    }
}

fn backend_configuration_changed(output: &crate::cmd::CmdOutput) -> bool {
    output.stdout().contains(BACKEND_CONFIGURATION_CHANGED)
        || output.stderr().contains(BACKEND_CONFIGURATION_CHANGED)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use camino_tempfile::Utf8TempDir;

    use super::*;

    #[test]
    fn apply_reinitializes_and_retries_when_backend_configuration_changed() {
        let temp = Utf8TempDir::new().unwrap();
        let cache_directory = temp.path().join(".terraform");
        fs_err::create_dir(&cache_directory).unwrap();

        let executable = temp.path().join("fake-terraform");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    if [ ! -f initialized ]; then
      printf 'Error: Backend configuration changed\n' >&2
      exit 1
    fi
    ;;
  init)
    if [ -d .terraform ]; then
      exit 2
    fi
    : > initialized
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();

        CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");

        assert!(!cache_directory.exists());
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false\napply\n"
        );
    }
}
