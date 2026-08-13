use std::collections::BTreeMap;

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};

use crate::{
    aws,
    cmd_runner::{CmdRunner, PlanOutcome},
    config::Config,
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

#[derive(Clone, Copy)]
enum ExecutionOrder {
    GroupedByAccount,
    PreserveDependencies,
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
            ExecutionOrder::GroupedByAccount,
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
            ExecutionOrder::GroupedByAccount,
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

    pub fn apply_all(&self, config: &Config) {
        self.for_each_authenticated_directory(
            config,
            LoginPolicy::Reuse,
            ExecutionOrder::PreserveDependencies,
            |cmd_runner, tool, directory| match tool {
                Tool::Terraform => cmd_runner.terraform_apply(directory),
                Tool::Terragrunt => cmd_runner.terragrunt_apply(directory),
            },
        );
    }

    fn for_each_authenticated_directory(
        &self,
        config: &Config,
        login_policy: LoginPolicy,
        execution_order: ExecutionOrder,
        mut operation: impl FnMut(&CmdRunner, Tool, &Utf8Path),
    ) {
        for batch in self.execution_batches(execution_order) {
            let account = batch.first().expect("empty execution batch").account();
            let cmd_runner = authenticated_cmd_runner(account, config, login_policy);
            for directory in batch {
                operation(&cmd_runner, directory.tool(), directory.path());
            }
        }
    }

    fn execution_batches(&self, execution_order: ExecutionOrder) -> Vec<Vec<&GroupedDir>> {
        match execution_order {
            ExecutionOrder::GroupedByAccount => {
                let mut by_account = BTreeMap::<&str, Vec<&GroupedDir>>::new();
                for directory in &self.directories {
                    by_account
                        .entry(directory.account())
                        .or_default()
                        .push(directory);
                }

                // Preserve the historical behavior of handling legacy credentials first.
                let mut batches = Vec::with_capacity(by_account.len());
                if let Some(legacy) = by_account.remove("legacy") {
                    batches.push(legacy);
                }
                batches.extend(by_account.into_values());
                batches
            }
            ExecutionOrder::PreserveDependencies => {
                let mut batches = Vec::<Vec<&GroupedDir>>::new();
                for directory in &self.directories {
                    if let Some(batch) = batches
                        .last_mut()
                        .filter(|batch| batch[0].account() == directory.account())
                    {
                        batch.push(directory);
                    } else {
                        batches.push(vec![directory]);
                    }
                }
                batches
            }
        }
    }
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
            .execution_batches(ExecutionOrder::GroupedByAccount)
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
    fn apply_order_preserves_dependency_order() {
        let directories = GroupedDirs {
            directories: vec![
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/a/first")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/b/only")).unwrap(),
                GroupedDir::new(Utf8Path::new("terragrunt/accounts/a/second")).unwrap(),
            ],
        };

        let ordered_paths = directories
            .execution_batches(ExecutionOrder::PreserveDependencies)
            .into_iter()
            .flatten()
            .map(|directory| directory.path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered_paths,
            vec![
                Utf8PathBuf::from("terragrunt/accounts/a/first"),
                Utf8PathBuf::from("terragrunt/accounts/b/only"),
                Utf8PathBuf::from("terragrunt/accounts/a/second"),
            ]
        );
    }
}
