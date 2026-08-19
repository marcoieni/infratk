use std::collections::BTreeMap;

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};

use crate::{
    aws,
    cmd_runner::{CmdRunner, PlanOutcome},
    config::Config,
    datadog,
    dir::{self, current_dir_is_simpleinfra},
};

/// Terraform and Terragrunt directories in execution order.
#[derive(Debug)]
pub struct GroupedDirs {
    directories: Vec<GroupedDir>,
}

#[derive(Debug)]
enum GroupedDir {
    Terraform(Utf8PathBuf),
    Terragrunt { account: String, path: Utf8PathBuf },
}

#[derive(Clone, Copy)]
enum Tool {
    Terraform,
    Terragrunt,
}

#[derive(Clone, Copy)]
enum LoginPolicy {
    Fresh,
    Reuse,
}

impl GroupedDirs {
    pub fn new<T>(directories: &[T]) -> Self
    where
        T: AsRef<Utf8Path>,
    {
        assert!(current_dir_is_simpleinfra());
        let directories = directories
            .iter()
            .filter_map(|directory| {
                let directory = directory.as_ref();
                let directory = if directory.is_absolute() {
                    directory.strip_prefix(dir::current_dir()).unwrap()
                } else {
                    directory
                };
                GroupedDir::new(directory)
            })
            .collect();
        Self { directories }
    }

    pub fn plan_all(&self, config: &Config) -> Vec<(Utf8PathBuf, PlanOutcome)> {
        let mut output = Vec::new();
        self.for_each_authenticated_directory(
            config,
            LoginPolicy::Fresh,
            |cmd_runner, tool, directory| {
                let outcome = match tool {
                    Tool::Terraform => cmd_runner.terraform_plan(directory),
                    Tool::Terragrunt => cmd_runner.terragrunt_plan(directory),
                };
                output.push((directory.to_path_buf(), outcome));
            },
        );
        output
    }

    pub fn upgrade_all(&self, config: &Config) -> Vec<(Utf8PathBuf, PlanOutcome)> {
        let mut output = Vec::new();
        self.for_each_authenticated_directory(
            config,
            LoginPolicy::Fresh,
            |cmd_runner, tool, directory| {
                let outcome = match tool {
                    Tool::Terraform => {
                        cmd_runner.terraform_init_upgrade(directory);
                        cmd_runner.terraform_plan(directory)
                    }
                    Tool::Terragrunt => {
                        cmd_runner.terragrunt_init_upgrade(directory);
                        cmd_runner.terragrunt_plan(directory)
                    }
                };
                output.push((directory.to_path_buf(), outcome));
            },
        );
        output
    }

    pub async fn apply_all(&self, config: &Config) {
        // Fetch and audit every live configuration before AWS authentication or
        // Terraform state changes. Unexpected Datadog settings are warned about
        // before the migration proceeds.
        let has_datadog_aws_migration = self.directories.iter().any(|directory| {
            matches!(directory.tool(), Tool::Terragrunt)
                && CmdRunner::is_datadog_aws_migration_target(directory.path())
        });
        let datadog_config_ids = if has_datadog_aws_migration {
            Some(
                datadog::load_aws_account_config_ids()
                    .await
                    .unwrap_or_else(|error| {
                        panic!("failed to prepare Datadog AWS state migration: {error:#}")
                    }),
            )
        } else {
            None
        };

        for batch in self.execution_batches() {
            let account = batch.first().expect("empty execution batch").account();
            let cmd_runner = authenticated_cmd_runner(account, config, LoginPolicy::Reuse);
            for directory in batch {
                match directory.tool() {
                    Tool::Terraform => cmd_runner.terraform_apply(directory.path()),
                    Tool::Terragrunt => {
                        if let Some(migration) =
                            cmd_runner.pending_datadog_aws_migration(directory.path())
                        {
                            let config_id = migration.requires_import().then(|| {
                                datadog_config_ids
                                    .as_ref()
                                    .expect("Datadog migration preflight was not run")
                                    .get(migration.aws_account_id())
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "Datadog has no integration config for AWS account {}",
                                            migration.aws_account_id()
                                        )
                                    })
                                    .to_string()
                            });
                            cmd_runner.complete_datadog_aws_migration(
                                directory.path(),
                                &migration,
                                config_id.as_deref(),
                            );
                        }
                        cmd_runner.terragrunt_apply(directory.path());
                    }
                }
            }
        }
    }

    fn for_each_authenticated_directory(
        &self,
        config: &Config,
        login_policy: LoginPolicy,
        mut operation: impl FnMut(&CmdRunner, Tool, &Utf8Path),
    ) {
        for batch in self.execution_batches() {
            let account = batch.first().expect("empty execution batch").account();
            let cmd_runner = authenticated_cmd_runner(account, config, login_policy);
            for directory in batch {
                operation(&cmd_runner, directory.tool(), directory.path());
            }
        }
    }

    fn execution_batches(&self) -> Vec<Vec<&GroupedDir>> {
        // Grouping by account is safe because simpleinfra has no cross-account dependencies.
        let mut by_account = BTreeMap::<&str, Vec<&GroupedDir>>::new();
        for directory in &self.directories {
            by_account
                .entry(directory.account())
                .or_default()
                .push(directory);
        }

        // Apply SSO accounts before switching to the legacy credentials.
        let legacy = by_account.remove("legacy");
        let mut batches = staging_before_production(by_account);
        if let Some(legacy) = legacy {
            batches.push(legacy);
        }
        batches
    }
}

fn staging_before_production<T>(mut by_account: BTreeMap<&str, T>) -> Vec<T> {
    let accounts = by_account
        .keys()
        .map(|account| (*account).to_owned())
        .collect::<Vec<_>>();
    let mut ordered = Vec::with_capacity(by_account.len());
    for account in accounts {
        if let Some(prefix) = account.strip_suffix("-prod") {
            let staging_account = format!("{prefix}-staging");
            if let Some(staging) = by_account.remove(staging_account.as_str()) {
                ordered.push(staging);
            }
        }
        if let Some(batch) = by_account.remove(account.as_str()) {
            ordered.push(batch);
        }
    }
    ordered
}

impl GroupedDir {
    fn new(directory: &Utf8Path) -> Option<Self> {
        let mut components = directory.components();
        match components.next() {
            Some(Utf8Component::Normal("terraform")) => {
                Some(Self::Terraform(directory.to_path_buf()))
            }
            Some(Utf8Component::Normal("terragrunt")) => {
                assert_eq!(components.next(), Some(Utf8Component::Normal("accounts")));
                let account = components.next().expect("missing Terragrunt account");
                Some(Self::Terragrunt {
                    account: account.to_string(),
                    path: directory.to_path_buf(),
                })
            }
            _ => None,
        }
    }

    fn account(&self) -> &str {
        match self {
            Self::Terraform(_) => "legacy",
            Self::Terragrunt { account, .. } => account,
        }
    }

    fn path(&self) -> &Utf8Path {
        match self {
            Self::Terraform(path) | Self::Terragrunt { path, .. } => path,
        }
    }

    fn tool(&self) -> Tool {
        match self {
            Self::Terraform(_) => Tool::Terraform,
            Self::Terragrunt { .. } => Tool::Terragrunt,
        }
    }
}

fn authenticated_cmd_runner(
    account: &str,
    config: &Config,
    login_policy: LoginPolicy,
) -> CmdRunner {
    let env_vars = match login_policy {
        LoginPolicy::Fresh => {
            // Logout before login to avoid conflicts between multiple profiles.
            aws::sso_logout();
            aws::login(account, config)
        }
        LoginPolicy::Reuse if account == "legacy" => {
            aws::legacy_login(config.op_legacy_item_id.as_deref())
        }
        LoginPolicy::Reuse => {
            aws::ensure_sso_login(account);
            BTreeMap::default()
        }
    };
    CmdRunner::new(env_vars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_grouping_is_independent_of_input_order() {
        let directories = GroupedDirs {
            directories: vec![
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/a/first")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/b/only")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/a/second")).unwrap(),
            ],
        };

        let grouped_paths = directories
            .execution_batches()
            .into_iter()
            .map(|batch| {
                batch
                    .into_iter()
                    .map(|directory| directory.path().to_path_buf())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            grouped_paths,
            vec![
                vec![
                    Utf8PathBuf::from("terragrunt/accounts/a/first"),
                    Utf8PathBuf::from("terragrunt/accounts/a/second"),
                ],
                vec![Utf8PathBuf::from("terragrunt/accounts/b/only")],
            ]
        );
    }

    #[test]
    fn staging_is_ordered_before_matching_production_account() {
        let directories = GroupedDirs {
            directories: vec![
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/bors-prod/only")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/other/only")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/bors-staging/only")).unwrap(),
            ],
        };

        let accounts = directories
            .execution_batches()
            .into_iter()
            .map(|batch| batch[0].account())
            .collect::<Vec<_>>();

        assert_eq!(accounts, vec!["bors-staging", "bors-prod", "other"]);
    }

    #[test]
    fn legacy_is_ordered_last() {
        let directories = GroupedDirs {
            directories: vec![
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/legacy/prod")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/bors-prod/only")).unwrap(),
                GroupedDir::new(Utf8Path::new("terraform/only")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/bors-staging/only")).unwrap(),
            ],
        };

        let accounts = directories
            .execution_batches()
            .into_iter()
            .map(|batch| batch[0].account())
            .collect::<Vec<_>>();

        assert_eq!(accounts, vec!["bors-staging", "bors-prod", "legacy"]);
    }
}
