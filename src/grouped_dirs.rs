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

    pub fn apply_all(&self, config: &Config) {
        self.for_each_authenticated_directory(
            config,
            LoginPolicy::Reuse,
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
        mut operation: impl FnMut(&CmdRunner, Tool, &Utf8Path),
    ) {
        let mut current_account = None;
        let mut cmd_runner = None;

        for directory in &self.directories {
            let account = directory.account();
            if current_account != Some(account) {
                cmd_runner = Some(authenticated_cmd_runner(account, config, login_policy));
                current_account = Some(account);
            }
            operation(
                cmd_runner.as_ref().unwrap(),
                directory.tool(),
                directory.path(),
            );
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
