use std::collections::BTreeMap;

use camino::Utf8Path;
use secrecy::SecretString;

use crate::{
    cmd::{Cmd, CmdOutput},
    git,
};

const BACKEND_CONFIG_DECODE_FAILED: &str = "Failed to decode current backend config";
const BACKEND_CONFIGURATION_CHANGED: &str = "Backend configuration changed";
const BACKEND_INITIALIZATION_REQUIRED: &str = "Backend initialization required";
const MAX_APPLY_RETRIES: usize = 2;

#[derive(Debug, PartialEq)]
enum BackendRecovery {
    ConfigurationChanged,
    InitializationRequired,
}

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
        let cache_directory = directory.join(cache_directory_name);
        for retry_count in 0..=MAX_APPLY_RETRIES {
            let output = Cmd::new(command, ["apply"])
                .with_env_vars(self.env_vars.clone())
                .with_current_dir(directory)
                .run_interactive_with_output();
            if output.status().success() {
                return;
            }

            if retry_count == MAX_APPLY_RETRIES {
                panic!("{command} apply failed after {MAX_APPLY_RETRIES} retries in {directory}");
            }

            let backend_recovery = backend_recovery(&output)
                .unwrap_or_else(|| panic!("{command} apply failed in {directory}"));

            match backend_recovery {
                BackendRecovery::ConfigurationChanged => {
                    match fs_err::remove_dir_all(&cache_directory) {
                        Ok(()) => println!("Removed stale cache directory {cache_directory}"),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => panic!("failed to remove {cache_directory}: {error}"),
                    }
                    println!("Backend configuration changed; reinitializing {directory}");
                }
                BackendRecovery::InitializationRequired => {
                    println!("Backend initialization required; initializing {directory}");
                }
            }

            let mut init_output = self.init_migrate_state(directory, command);
            if !init_output.status().success()
                && output_contains(&init_output, BACKEND_CONFIG_DECODE_FAILED)
                && remove_unset_cached_backend_settings(&cache_directory)
            {
                println!("Cached backend configuration updated; initializing {directory} again");
                init_output = self.init_migrate_state(directory, command);
            }
            assert!(
                init_output.status().success(),
                "{command} init failed in {directory}"
            );

            let git_status = git::working_tree_status(directory).unwrap_or_else(|error| {
                panic!(
                    "failed to verify the repository after {command} init in {directory}: {error}"
                )
            });
            assert!(
                git_status.is_empty(),
                "{command} init left the repository dirty; refusing to retry {command} apply:\n{git_status}"
            );
        }

        unreachable!("apply retry loop always returns or panics");
    }

    fn init_migrate_state(&self, directory: &Utf8Path, command: &str) -> CmdOutput {
        // `-migrate-state` lets `init` accept an edited backend block. With `-input=false`,
        // it does so only when no state of the cached backend has to be copied (e.g. only
        // the locking changed), and fails otherwise.
        Cmd::new(command, ["init", "-input=false", "-migrate-state"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .run()
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

fn backend_recovery(output: &CmdOutput) -> Option<BackendRecovery> {
    if output_contains(output, BACKEND_CONFIGURATION_CHANGED) {
        Some(BackendRecovery::ConfigurationChanged)
    } else if output_contains(output, BACKEND_INITIALIZATION_REQUIRED) {
        Some(BackendRecovery::InitializationRequired)
    } else {
        None
    }
}

fn output_contains(output: &CmdOutput, message: &str) -> bool {
    output.stdout().contains(message) || output.stderr().contains(message)
}

/// Remove the unset settings from the backend configurations that `init` cached in
/// `cache_directory`. Returns whether any setting was removed.
///
/// Terraform can't decode a cached backend configuration that contains a setting
/// its backend no longer supports, even if the setting is unset, e.g. the
/// `assume_role_duration_seconds` that older Terraform versions cached for the
/// S3 backend. Terraform reads omitted settings as unset, so removing the unset
/// settings doesn't change the cached configuration, which `init -migrate-state`
/// needs to detect whether the state would move.
fn remove_unset_cached_backend_settings(cache_directory: &Utf8Path) -> bool {
    if !cache_directory.exists() {
        return false;
    }

    let mut removed_settings = false;
    // Terragrunt runs Terraform in a copy of the module inside its cache, so search
    // it recursively, including the hidden and git-ignored `.terraform` directories.
    let walker = ignore::WalkBuilder::new(cache_directory)
        .standard_filters(false)
        .build();
    for entry in walker {
        let entry = entry.expect("invalid entry");
        if !entry.path().ends_with(".terraform/terraform.tfstate") {
            continue;
        }

        let path = Utf8Path::from_path(entry.path()).expect("non UTF-8 path");
        let content = fs_err::read_to_string(path).unwrap();
        let mut backend_state: serde_json::Value = serde_json::from_str(&content)
            .unwrap_or_else(|error| panic!("failed to parse {path}: {error}"));
        let Some(config) = backend_state
            .pointer_mut("/backend/config")
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };

        let settings_count = config.len();
        config.retain(|_, value| !value.is_null());
        if config.len() < settings_count {
            let content = serde_json::to_string_pretty(&backend_state).unwrap();
            fs_err::write(path, content).unwrap();
            println!("Removed unset settings from the cached backend configuration {path}");
            removed_settings = true;
        }
    }
    removed_settings
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    use camino_tempfile::Utf8TempDir;

    use super::*;

    fn commit_test_repository(directory: &Utf8Path) {
        fs_err::write(
            directory.join(".gitignore"),
            "calls\ninitialized\n.terraform/\n.terragrunt-cache/\n",
        )
        .unwrap();

        for args in [
            &["init"][..],
            &["config", "user.name", "Test User"],
            &["config", "user.email", "test@example.com"],
            &["add", "."],
            &["commit", "-m", "test fixture"],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(directory)
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
        commit_test_repository(temp.path());

        CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");

        assert!(!cache_directory.exists());
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\napply\n"
        );
    }

    #[test]
    fn apply_initializes_and_retries_when_backend_initialization_is_required() {
        let temp = Utf8TempDir::new().unwrap();
        let cache_directory = temp.path().join(".terragrunt-cache");
        fs_err::create_dir(&cache_directory).unwrap();

        let executable = temp.path().join("fake-terragrunt");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    if [ ! -f initialized ]; then
      printf 'Error: Backend initialization required, please run "terraform init"\n' >&2
      exit 1
    fi
    ;;
  init)
    : > initialized
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        CmdRunner::new(BTreeMap::new()).apply(
            temp.path(),
            executable.as_str(),
            ".terragrunt-cache",
        );

        assert!(cache_directory.exists());
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\napply\n"
        );
    }

    #[test]
    fn apply_reinitializes_when_backend_change_keeps_the_same_state() {
        let temp = Utf8TempDir::new().unwrap();
        // The cache directory stores the backend settings of the previous
        // configuration, e.g. before switching from DynamoDB to S3 locking.
        let cache_directory = temp.path().join(".terraform");
        fs_err::create_dir(&cache_directory).unwrap();

        // Mimics Terraform after the backend block was edited: `apply` only asks
        // for initialization, and `init` refuses with "Backend configuration
        // changed" unless `-migrate-state` tells it how to handle the state.
        // Because the state stays in place, `-migrate-state` lets init succeed.
        let executable = temp.path().join("fake-terraform");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    if [ ! -f initialized ]; then
      printf 'Error: Backend initialization required, please run "terraform init"\n\nReason: Backend configuration block has changed\n' >&2
      exit 1
    fi
    ;;
  init)
    case " $* " in
      *" -migrate-state "*) ;;
      *)
        printf 'Error: Backend configuration changed\n' >&2
        exit 1
        ;;
    esac
    : > initialized
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");

        // Keeping the cache preserves the providers, modules and selected workspace.
        assert!(cache_directory.exists());
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\napply\n"
        );
    }

    #[test]
    fn apply_stops_when_backend_change_would_move_the_state() {
        let temp = Utf8TempDir::new().unwrap();
        let cache_directory = temp.path().join(".terraform");
        fs_err::create_dir(&cache_directory).unwrap();

        // Mimics Terraform after the backend block was edited to point to a
        // different state, e.g. another bucket or key. With input disabled,
        // `init -migrate-state` can't ask for approval to copy the state, so it
        // fails. Without a cached backend there is nothing to compare against,
        // so init would silently attach to the new, possibly empty, state.
        // Like Terraform, the refusing init first prints "Backend configuration
        // changed!", so this test catches a fallback that deletes the cache and
        // runs init again when init prints that message.
        let executable = temp.path().join("fake-terraform");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    if [ ! -f initialized ]; then
      printf 'Error: Backend initialization required, please run "terraform init"\n\nReason: Backend configuration block has changed\n' >&2
      exit 1
    fi
    ;;
  init)
    if [ ! -d .terraform ]; then
      : > initialized
      exit 0
    fi
    case " $* " in
      *" -migrate-state "*)
        printf 'Backend configuration changed!\n'
        printf "Error: Can't ask approval for state migration when interactive input is disabled.\n" >&2
        ;;
      *)
        printf 'Error: Backend configuration changed\n' >&2
        ;;
    esac
    exit 1
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        let result = std::panic::catch_unwind(|| {
            CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");
        });

        // Moving the state needs a manual migration, so apply must stop
        // before running against the new state.
        let panic = result.expect_err("apply should stop when the state would move");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("init failed"));
        // The cached backend settings are what lets init detect the state move
        // on the next run, so they must be left in place.
        assert!(cache_directory.exists());
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\n"
        );
    }

    /// Backend configuration cached by `init` with a Terraform version whose S3
    /// backend still supported `assume_role_duration_seconds`.
    fn legacy_backend_state(assume_role_duration_seconds: Option<u64>) -> serde_json::Value {
        serde_json::json!({
            "version": 3,
            "serial": 1,
            "lineage": "00000000-0000-0000-0000-000000000000",
            "backend": {
                "type": "s3",
                "config": {
                    "access_key": null,
                    "assume_role_duration_seconds": assume_role_duration_seconds,
                    "bucket": "rust-terraform",
                    "dynamodb_table": "terraform-state-lock",
                    "encrypt": true,
                    "key": "simpleinfra/bastion.tfstate",
                    "region": "us-west-1"
                },
                "hash": 3_591_436_254_u64
            },
            "modules": [{ "path": ["root"], "outputs": {}, "resources": {}, "depends_on": [] }]
        })
    }

    /// Mimics a newer Terraform after the backend block was edited: like
    /// Terraform, init fails to decode the cached backend configuration
    /// while it contains a setting that the backend no longer supports,
    /// even if the setting is unset. It searches both the Terraform and the
    /// Terragrunt cache, so it can stand in for either command.
    const FAKE_NEWER_TERRAFORM: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    if [ ! -f initialized ]; then
      printf 'Error: Backend initialization required, please run "terraform init"\n\nReason: Backend configuration block has changed\n' >&2
      exit 1
    fi
    ;;
  init)
    if grep -rqs '"assume_role_duration_seconds"' .terraform .terragrunt-cache; then
      printf 'Error: Failed to decode current backend config\n\nThe backend configuration created by the most recent run of "terraform init" could not be decoded: unsupported attribute "assume_role_duration_seconds".\n' >&2
      exit 1
    fi
    : > initialized
    ;;
esac
"#;

    fn read_json(path: &Utf8Path) -> serde_json::Value {
        serde_json::from_str(&fs_err::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn apply_removes_unsupported_unset_settings_from_cached_backend_configuration() {
        let temp = Utf8TempDir::new().unwrap();
        let backend_state_path = temp.path().join(".terraform/terraform.tfstate");
        fs_err::create_dir(temp.path().join(".terraform")).unwrap();
        let legacy_state = legacy_backend_state(None);
        fs_err::write(&backend_state_path, legacy_state.to_string()).unwrap();

        let executable = temp.path().join("fake-terraform");
        fs_err::write(&executable, FAKE_NEWER_TERRAFORM).unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");

        // `init -migrate-state` compares the cached backend configuration with
        // the edited backend block to detect whether the state would move, so
        // only the unset settings may be removed.
        let mut expected_state = legacy_state;
        let config = expected_state["backend"]["config"].as_object_mut().unwrap();
        config.remove("access_key");
        config.remove("assume_role_duration_seconds");
        assert_eq!(read_json(&backend_state_path), expected_state);
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\ninit -input=false -migrate-state\napply\n"
        );
    }

    #[test]
    fn apply_removes_unsupported_unset_settings_from_terragrunt_cache() {
        let temp = Utf8TempDir::new().unwrap();
        // Terragrunt runs Terraform in a copy of the module inside its cache,
        // so the backend configuration is cached in a nested `.terraform`.
        let terraform_directory = temp.path().join(".terragrunt-cache/abc/def/.terraform");
        fs_err::create_dir_all(&terraform_directory).unwrap();
        let backend_state_path = terraform_directory.join("terraform.tfstate");
        fs_err::write(&backend_state_path, legacy_backend_state(None).to_string()).unwrap();

        let executable = temp.path().join("fake-terragrunt");
        fs_err::write(&executable, FAKE_NEWER_TERRAFORM).unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        CmdRunner::new(BTreeMap::new()).apply(
            temp.path(),
            executable.as_str(),
            ".terragrunt-cache",
        );

        assert!(read_json(&backend_state_path)["backend"]["config"]
            .get("assume_role_duration_seconds")
            .is_none());
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\ninit -input=false -migrate-state\napply\n"
        );
    }

    #[test]
    fn apply_stops_when_unsupported_cached_backend_setting_is_set() {
        let temp = Utf8TempDir::new().unwrap();
        let backend_state_path = temp.path().join(".terraform/terraform.tfstate");
        fs_err::create_dir(temp.path().join(".terraform")).unwrap();
        fs_err::write(
            &backend_state_path,
            legacy_backend_state(Some(3600)).to_string(),
        )
        .unwrap();

        let executable = temp.path().join("fake-terraform");
        fs_err::write(&executable, FAKE_NEWER_TERRAFORM).unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        let result = std::panic::catch_unwind(|| {
            CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");
        });

        // A setting that is set affects how Terraform reads the state of the
        // cached backend, so it must be kept and the user has to reinitialize
        // the backend manually, as Terraform's error explains.
        let panic = result.expect_err("apply should stop when the cached setting is set");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("init failed"));
        assert_eq!(
            read_json(&backend_state_path)["backend"]["config"]["assume_role_duration_seconds"],
            3600
        );
        // Init runs again only because the unset `access_key` was removed.
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\ninit -input=false -migrate-state\n"
        );
    }

    #[test]
    fn apply_keeps_cached_backend_configuration_when_init_fails_for_another_reason() {
        let temp = Utf8TempDir::new().unwrap();
        let backend_state_path = temp.path().join(".terraform/terraform.tfstate");
        fs_err::create_dir(temp.path().join(".terraform")).unwrap();
        // The unset settings would be removed if infratk cleaned up the cache
        // whenever init fails.
        let legacy_state = legacy_backend_state(None).to_string();
        fs_err::write(&backend_state_path, &legacy_state).unwrap();

        // Init decodes the cached backend configuration, but refuses to move
        // the state, as in `apply_stops_when_backend_change_would_move_the_state`.
        let executable = temp.path().join("fake-terraform");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    printf 'Error: Backend initialization required, please run "terraform init"\n\nReason: Backend configuration block has changed\n' >&2
    exit 1
    ;;
  init)
    printf "Error: Can't ask approval for state migration when interactive input is disabled.\n" >&2
    exit 1
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        let result = std::panic::catch_unwind(|| {
            CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");
        });

        let panic = result.expect_err("apply should stop when init fails");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("init failed"));
        // infratk should only touch Terraform's cache when Terraform refused to
        // decode it, so the file must not even be rewritten and init must not
        // be retried.
        assert_eq!(
            fs_err::read_to_string(&backend_state_path).unwrap(),
            legacy_state
        );
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\n"
        );
    }

    #[test]
    fn apply_recovers_when_backend_configuration_changes_after_initialization() {
        let temp = Utf8TempDir::new().unwrap();

        let executable = temp.path().join("fake-terraform");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    apply_count="$(grep -c '^apply$' calls)"
    if [ "$apply_count" -eq 1 ]; then
      printf 'Error: Backend initialization required\n' >&2
      exit 1
    fi
    if [ "$apply_count" -eq 2 ]; then
      printf 'Error: Backend configuration changed\n' >&2
      exit 1
    fi
    ;;
  init)
    init_count="$(grep -c '^init ' calls)"
    if [ "$init_count" -eq 2 ] && [ -d .terraform ]; then
      exit 2
    fi
    mkdir -p .terraform
    : > initialized
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");

        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\napply\ninit -input=false -migrate-state\napply\n"
        );
    }

    #[test]
    fn apply_stops_after_two_retries() {
        let temp = Utf8TempDir::new().unwrap();

        let executable = temp.path().join("fake-terraform");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    printf 'Error: Backend initialization required\n' >&2
    exit 1
    ;;
  init)
    : > initialized
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        let result = std::panic::catch_unwind(|| {
            CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");
        });

        let panic = result.expect_err("apply should stop after two retries");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("apply failed after 2 retries"));
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\napply\ninit -input=false -migrate-state\napply\n"
        );
    }

    #[test]
    fn apply_does_not_retry_when_init_changes_the_repository() {
        let temp = Utf8TempDir::new().unwrap();
        fs_err::write(temp.path().join(".terraform.lock.hcl"), "original\n").unwrap();

        let executable = temp.path().join("fake-terraform");
        fs_err::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' "$*" >> calls
case "$1" in
  apply)
    printf 'Error: Backend initialization required\n' >&2
    exit 1
    ;;
  init)
    printf 'updated\n' >> .terraform.lock.hcl
    ;;
esac
"#,
        )
        .unwrap();
        let mut permissions = fs_err::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs_err::set_permissions(&executable, permissions).unwrap();
        commit_test_repository(temp.path());

        let result = std::panic::catch_unwind(|| {
            CmdRunner::new(BTreeMap::new()).apply(temp.path(), executable.as_str(), ".terraform");
        });

        let panic = result.expect_err("apply should abort when init changes the repository");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("init left the repository dirty"));
        assert!(message.contains(".terraform.lock.hcl"));
        assert_eq!(
            fs_err::read_to_string(temp.path().join("calls")).unwrap(),
            "apply\ninit -input=false -migrate-state\n"
        );
    }
}
