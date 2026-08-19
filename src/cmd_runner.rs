use std::collections::BTreeMap;

use anyhow::{bail, Context as _};
use camino::Utf8Path;
use secrecy::SecretString;

use crate::cmd::Cmd;

const BACKEND_CONFIGURATION_CHANGED: &str = "Backend configuration changed";
const DATADOG_AWS_STATE_DIRECTORY: &str = "datadog-aws";
const DATADOG_AWS_MODULE_CONFIG: &str = "terragrunt/modules/datadog-aws/main.tf";
const OLD_DATADOG_AWS_ADDRESS: &str = "datadog_integration_aws.aws";
const OLD_DATADOG_AWS_TYPE: &str = "datadog_integration_aws";
const NEW_DATADOG_AWS_ADDRESS: &str = "datadog_integration_aws_account.aws";
const NEW_DATADOG_AWS_TYPE: &str = "datadog_integration_aws_account";

#[derive(Debug, PartialEq)]
pub enum PlanOutcome {
    NoChanges,
    Changes(String),
}

#[derive(Debug, PartialEq)]
pub struct PendingDatadogAwsMigration {
    aws_account_id: String,
    requires_import: bool,
}

impl PendingDatadogAwsMigration {
    pub fn aws_account_id(&self) -> &str {
        &self.aws_account_id
    }

    pub fn requires_import(&self) -> bool {
        self.requires_import
    }
}

#[derive(Debug, PartialEq)]
struct DatadogAwsState {
    old_account_id: Option<String>,
    new_account_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct TerraformState {
    #[serde(default)]
    resources: Vec<StateResource>,
}

#[derive(serde::Deserialize)]
struct StateResource {
    module: Option<String>,
    mode: String,
    #[serde(rename = "type")]
    resource_type: String,
    name: String,
    #[serde(default)]
    instances: Vec<StateInstance>,
}

#[derive(serde::Deserialize)]
struct StateInstance {
    index_key: Option<serde_json::Value>,
    attributes: serde_json::Value,
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

    /// Detect the state-only steps required when replacing the deprecated
    /// Datadog AWS integration resource with the unified account resource.
    pub fn is_datadog_aws_migration_target(state: &Utf8Path) -> bool {
        if state.file_name() != Some(DATADOG_AWS_STATE_DIRECTORY) {
            return false;
        }
        let module_config = fs_err::read_to_string(DATADOG_AWS_MODULE_CONFIG)
            .unwrap_or_else(|error| panic!("failed to read {DATADOG_AWS_MODULE_CONFIG}: {error}"));
        module_config.contains(r#"resource "datadog_integration_aws_account" "aws""#)
    }

    /// Detect the state-only steps required when replacing the deprecated
    /// Datadog AWS integration resource with the unified account resource.
    pub fn pending_datadog_aws_migration(
        &self,
        state: &Utf8Path,
    ) -> Option<PendingDatadogAwsMigration> {
        if !Self::is_datadog_aws_migration_target(state) {
            return None;
        }

        let datadog_state = self.datadog_aws_state(state);
        pending_datadog_aws_migration(&datadog_state).unwrap_or_else(|error| {
            panic!("invalid Datadog AWS integration state in {state}: {error:#}")
        })
    }

    /// Import the existing Datadog account under its new address before
    /// removing the deprecated address. Every transition is validated so a
    /// failed or interrupted run can safely be resumed.
    pub fn complete_datadog_aws_migration(
        &self,
        state: &Utf8Path,
        migration: &PendingDatadogAwsMigration,
        config_id: Option<&str>,
    ) {
        println!(
            "Migrating Datadog AWS integration state for account {} in {state}",
            migration.aws_account_id
        );

        if migration.requires_import {
            let config_id = config_id.expect("Datadog config ID is required for import");
            let output = Cmd::new(
                "terragrunt",
                ["import", "-input=false", NEW_DATADOG_AWS_ADDRESS, config_id],
            )
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(state)
            .run();
            assert!(
                output.status().success(),
                "failed to import {NEW_DATADOG_AWS_ADDRESS} in {state}"
            );

            let imported_state = self.datadog_aws_state(state);
            assert_eq!(
                imported_state,
                DatadogAwsState {
                    old_account_id: Some(migration.aws_account_id.clone()),
                    new_account_id: Some(migration.aws_account_id.clone()),
                },
                "the Datadog resource import produced unexpected state in {state}"
            );
        } else {
            assert!(
                config_id.is_none(),
                "a Datadog config ID was provided for an already imported resource"
            );
        }

        let output = Cmd::new("terragrunt", ["state", "rm", OLD_DATADOG_AWS_ADDRESS])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(state)
            .run();
        assert!(
            output.status().success(),
            "failed to remove {OLD_DATADOG_AWS_ADDRESS} from state in {state}"
        );

        assert_eq!(
            self.datadog_aws_state(state),
            DatadogAwsState {
                old_account_id: None,
                new_account_id: Some(migration.aws_account_id.clone()),
            },
            "the Datadog state migration produced unexpected state in {state}"
        );
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

        self.remove_cache_and_reinitialize(directory, command, cache_directory_name);

        let retry_status = Cmd::new(command, ["apply"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .run_interactive();
        assert!(
            retry_status.success(),
            "{command} apply failed after reinitializing {directory}"
        );
    }

    fn datadog_aws_state(&self, directory: &Utf8Path) -> DatadogAwsState {
        let state = self.terragrunt_state_pull(directory);
        parse_datadog_aws_state(&state).unwrap_or_else(|error| {
            panic!("failed to parse Terraform state in {directory}: {error:#}")
        })
    }

    fn terragrunt_state_pull(&self, directory: &Utf8Path) -> String {
        let output = self.terragrunt_state_pull_once(directory);
        let output = if output.status().success() {
            output
        } else {
            assert!(
                backend_configuration_changed(&output),
                "terragrunt state pull failed in {directory}"
            );
            self.remove_cache_and_reinitialize(directory, "terragrunt", ".terragrunt-cache");
            self.terragrunt_state_pull_once(directory)
        };
        assert!(
            output.status().success(),
            "terragrunt state pull failed after reinitializing {directory}"
        );
        output.stdout().to_string()
    }

    fn terragrunt_state_pull_once(&self, directory: &Utf8Path) -> crate::cmd::CmdOutput {
        Cmd::new("terragrunt", ["state", "pull"])
            .with_env_vars(self.env_vars.clone())
            .with_current_dir(directory)
            .hide_stdout()
            .run()
    }

    fn remove_cache_and_reinitialize(
        &self,
        directory: &Utf8Path,
        command: &str,
        cache_directory_name: &str,
    ) {
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

fn parse_datadog_aws_state(state: &str) -> anyhow::Result<DatadogAwsState> {
    let state = serde_json::from_str::<TerraformState>(state)
        .context("state pull did not return valid Terraform state JSON")?;
    Ok(DatadogAwsState {
        old_account_id: account_id_for_resource(&state, OLD_DATADOG_AWS_TYPE, "account_id")?,
        new_account_id: account_id_for_resource(&state, NEW_DATADOG_AWS_TYPE, "aws_account_id")?,
    })
}

fn account_id_for_resource(
    state: &TerraformState,
    resource_type: &str,
    account_id_attribute: &str,
) -> anyhow::Result<Option<String>> {
    let resources = state
        .resources
        .iter()
        .filter(|resource| {
            resource.mode == "managed"
                && resource.resource_type == resource_type
                && resource.name == "aws"
        })
        .collect::<Vec<_>>();
    let Some(resource) = resources.first() else {
        return Ok(None);
    };
    if resources.len() != 1 {
        bail!("state contains multiple {resource_type}.aws resources");
    }
    if resource.module.is_some() {
        bail!("{resource_type}.aws is unexpectedly inside a child module");
    }
    if resource.instances.len() != 1 || resource.instances[0].index_key.is_some() {
        bail!("{resource_type}.aws is not a singleton resource");
    }

    let account_id = resource.instances[0]
        .attributes
        .get(account_id_attribute)
        .and_then(serde_json::Value::as_str)
        .with_context(|| {
            format!("{resource_type}.aws has no string {account_id_attribute} attribute")
        })?;
    if account_id.len() != 12 || !account_id.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("{resource_type}.aws has invalid AWS account ID {account_id:?}");
    }
    Ok(Some(account_id.to_string()))
}

fn pending_datadog_aws_migration(
    state: &DatadogAwsState,
) -> anyhow::Result<Option<PendingDatadogAwsMigration>> {
    let Some(old_account_id) = &state.old_account_id else {
        return Ok(None);
    };
    if let Some(new_account_id) = &state.new_account_id {
        if new_account_id != old_account_id {
            bail!(
                "old resource belongs to AWS account {old_account_id}, but new resource belongs to {new_account_id}"
            );
        }
    }
    Ok(Some(PendingDatadogAwsMigration {
        aws_account_id: old_account_id.clone(),
        requires_import: state.new_account_id.is_none(),
    }))
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

    #[test]
    fn detects_datadog_resource_that_requires_import() {
        let state = parse_datadog_aws_state(&state_json(Some("012345678901"), None)).unwrap();

        assert_eq!(
            pending_datadog_aws_migration(&state).unwrap(),
            Some(PendingDatadogAwsMigration {
                aws_account_id: "012345678901".to_string(),
                requires_import: true,
            })
        );
    }

    #[test]
    fn resumes_after_datadog_resource_was_already_imported() {
        let state =
            parse_datadog_aws_state(&state_json(Some("012345678901"), Some("012345678901")))
                .unwrap();

        assert_eq!(
            pending_datadog_aws_migration(&state).unwrap(),
            Some(PendingDatadogAwsMigration {
                aws_account_id: "012345678901".to_string(),
                requires_import: false,
            })
        );
    }

    #[test]
    fn rejects_datadog_resources_for_different_accounts() {
        let state =
            parse_datadog_aws_state(&state_json(Some("012345678901"), Some("109876543210")))
                .unwrap();

        let error = pending_datadog_aws_migration(&state).unwrap_err();
        assert!(error.to_string().contains("but new resource belongs to"));
    }

    #[test]
    fn ignores_state_after_datadog_migration_is_complete() {
        let state = parse_datadog_aws_state(&state_json(None, Some("012345678901"))).unwrap();
        assert_eq!(pending_datadog_aws_migration(&state).unwrap(), None);
    }

    fn state_json(old_account_id: Option<&str>, new_account_id: Option<&str>) -> String {
        let mut resources = Vec::new();
        if let Some(account_id) = old_account_id {
            resources.push(serde_json::json!({
                "mode": "managed",
                "type": OLD_DATADOG_AWS_TYPE,
                "name": "aws",
                "instances": [{
                    "attributes": { "account_id": account_id }
                }]
            }));
        }
        if let Some(account_id) = new_account_id {
            resources.push(serde_json::json!({
                "mode": "managed",
                "type": NEW_DATADOG_AWS_TYPE,
                "name": "aws",
                "instances": [{
                    "attributes": { "aws_account_id": account_id }
                }]
            }));
        }
        serde_json::json!({ "resources": resources }).to_string()
    }
}
