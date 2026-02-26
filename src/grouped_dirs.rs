use std::collections::BTreeMap;

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};

use crate::{
    aws,
    cmd_runner::{CmdRunner, PlanOutcome},
    config::Config,
    dir::{self, current_dir_is_simpleinfra},
};

/// Directoried grouped by type and account
#[derive(Debug)]
pub struct GroupedDirs {
    /// Directories under the terraform directory
    terraform: Vec<Utf8PathBuf>,
    /// Directories under the terragrunt directory, grouped by account
    terragrunt: BTreeMap<String, Vec<Utf8PathBuf>>,
}

impl GroupedDirs {
    pub fn new<T>(directories: &[T]) -> Self
    where
        T: AsRef<Utf8Path>,
    {
        assert!(current_dir_is_simpleinfra());
        let directories: Vec<&Utf8Path> = directories
            .iter()
            .map(|d| {
                let dir = d.as_ref();
                if dir.is_absolute() {
                    dir.strip_prefix(dir::current_dir()).unwrap()
                } else {
                    dir
                }
            })
            .collect();

        let terragrunt_dirs: Vec<&Utf8Path> =
            get_dirs_starting_with(directories.clone(), "terragrunt");
        let terraform_dirs: Vec<&Utf8Path> =
            get_dirs_starting_with(directories.clone(), "terraform");
        let grouped_terragrunt_dirs = group_terragrunt_dirs_by_account(terragrunt_dirs);
        Self {
            terraform: terraform_dirs
                .into_iter()
                .map(|d| d.to_path_buf())
                .collect(),
            terragrunt: grouped_terragrunt_dirs
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(|d| d.to_path_buf()).collect()))
                .collect(),
        }
    }

    pub fn contains_legacy_account(&self) -> bool {
        self.terragrunt.contains_key("legacy") || !self.terraform.is_empty()
    }

    pub fn terraform_dirs(&self) -> Vec<&Utf8Path> {
        self.terraform
            .iter()
            .map(|d| d.as_path())
            .collect::<Vec<_>>()
    }

    pub fn legacy_terragrunt_dirs(&self) -> Vec<Utf8PathBuf> {
        self.terragrunt.get("legacy").cloned().unwrap_or_default()
    }

    /// Returns a map of account names to directories.
    /// Legacy account is excluded.
    pub fn sso_terragrunt_dirs(&self) -> BTreeMap<&str, Vec<Utf8PathBuf>> {
        self.terragrunt
            .iter()
            .filter(|(account, _)| !account.starts_with("legacy"))
            .map(|(account, dirs)| (account.as_str(), dirs.clone()))
            .collect()
    }

    pub fn upgrade_all(&self, config: &Config) -> Vec<(Utf8PathBuf, PlanOutcome)> {
        let mut output: Vec<(Utf8PathBuf, PlanOutcome)> = vec![];
        if self.contains_legacy_account() {
            let legacy_tg_dirs = self.legacy_terragrunt_dirs();
            let plan_outcome = upgrade_legacy_dirs(self.terraform_dirs(), legacy_tg_dirs, config);
            output.extend(plan_outcome);
        }

        let sso_terragrunt_dirs = self.sso_terragrunt_dirs();
        let plan_outcome = upgrade_terragrunt_with_sso(&sso_terragrunt_dirs);
        output.extend(plan_outcome);
        output
    }
}

fn get_dirs_starting_with<'a>(directories: Vec<&'a Utf8Path>, name: &str) -> Vec<&'a Utf8Path> {
    directories
        .into_iter()
        .filter(|&d| is_root_dir(d, name))
        .collect()
}

fn is_root_dir(dir: &Utf8Path, name: &str) -> bool {
    dir.components().next() == Some(Utf8Component::Normal(name))
}

fn group_terragrunt_dirs_by_account(
    terragrunt_dirs: Vec<&Utf8Path>,
) -> BTreeMap<String, Vec<&Utf8Path>> {
    let mut dirs = BTreeMap::new();
    for d in terragrunt_dirs {
        let mut components = d.components();
        let terragrunt_dir = components.next().unwrap();
        assert_eq!(terragrunt_dir, Utf8Component::Normal("terragrunt"));
        let accounts_dir = components.next().unwrap();
        assert_eq!(accounts_dir, Utf8Component::Normal("accounts"));
        let account = components.next().unwrap();
        // Add the directory to the account's list of directories.
        // If the account does not exist, create a new list with the directory.
        // If the account exists, append the directory to the list.
        dirs.entry(account.to_string())
            .or_insert_with(Vec::new)
            .push(d);
    }
    dirs
}

fn upgrade_legacy_dirs<T, U>(
    terraform_dirs: Vec<T>,
    terragrunt_dirs: Vec<U>,
    config: &Config,
) -> Vec<(Utf8PathBuf, PlanOutcome)>
where
    T: AsRef<Utf8Path>,
    U: AsRef<Utf8Path>,
{
    // logout before login, to avoid issues with multiple profiles
    aws::sso_logout();
    let login_env_vars = aws::legacy_login(config.op_legacy_item_id.as_deref());
    let cmd_runner = CmdRunner::new(login_env_vars);

    let mut outcome = vec![];
    for d in terraform_dirs {
        let d = d.as_ref();
        cmd_runner.terraform_init_upgrade(d);
        let plan_outcome = cmd_runner.terraform_plan(d);
        outcome.push((d.to_path_buf(), plan_outcome));
    }
    for d in terragrunt_dirs {
        let d = d.as_ref();
        cmd_runner.terragrunt_init_upgrade(d);
        let plan_outcome = cmd_runner.terragrunt_plan(d);
        outcome.push((d.to_path_buf(), plan_outcome));
    }
    outcome
}

fn upgrade_terragrunt_with_sso<T>(
    terragrunt_sso_dirs: &BTreeMap<&str, Vec<T>>,
) -> Vec<(Utf8PathBuf, PlanOutcome)>
where
    T: AsRef<Utf8Path>,
{
    let mut outcome = vec![];
    let terragrunt_sso_dirs = terragrunt_sso_dirs
        .iter()
        .map(|(k, v)| (*k, v.iter().map(|d| d.as_ref()).collect::<Vec<_>>()))
        .collect::<BTreeMap<_, _>>();
    for (account, dirs) in terragrunt_sso_dirs {
        aws::sso_logout();
        aws::sso_login(account);
        let cmd_runner = CmdRunner::new(BTreeMap::new());
        for d in dirs {
            cmd_runner.terragrunt_init_upgrade(d);
            let plan_outcome = cmd_runner.terragrunt_plan(d);
            outcome.push((d.to_path_buf(), plan_outcome));
        }
    }
    outcome
}
