use std::collections::BTreeSet;

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use inquire::Select;

use crate::{
    aws, command::legacy_login::login_to_legacy_aws_account, config::Config,
    dir::current_dir_is_simpleinfra, graph,
};

const LEGACY_AWS_ENV_VARS: [&str; 4] = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_SECURITY_TOKEN",
];

pub fn cd(config: &Config) {
    assert!(current_dir_is_simpleinfra());
    let modules = graph::get_all_modules();
    let module = select_module(modules);
    let account = module_account(&module);

    unset_legacy_env_vars();
    match account {
        Account::Legacy => {
            login_to_legacy_aws_account(config);
        }
        Account::Sso(account) => {
            aws::sso_login_quiet(&account);
            let profile = aws::sso_profile(&account);
            println!("export AWS_PROFILE={}", shell_quote(&profile));
        }
    }

    println!("cd {}", shell_quote(module.as_str()));
}

fn unset_legacy_env_vars() {
    for key in LEGACY_AWS_ENV_VARS {
        println!("unset {key}");
    }
}

fn select_module(modules: Vec<Utf8PathBuf>) -> Utf8PathBuf {
    Select::new("Select a module:", modules)
        .prompt()
        .unwrap_or_else(|e| panic!("failed to select module: {e:?}"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Debug, PartialEq, Eq)]
enum Account {
    Legacy,
    Sso(String),
}

fn module_account(module: &Utf8Path) -> Account {
    let mut components = module.components();
    match components.next() {
        Some(Utf8Component::Normal("terraform")) => Account::Legacy,
        Some(Utf8Component::Normal("terragrunt")) => {
            let accounts = components.next();
            assert_eq!(
                accounts,
                Some(Utf8Component::Normal("accounts")),
                "invalid terragrunt module: {module}"
            );
            let account = components
                .next()
                .expect("missing account in terragrunt path");
            if account == Utf8Component::Normal("legacy") {
                Account::Legacy
            } else {
                Account::Sso(account.to_string())
            }
        }
        _ => panic!("module is not under terraform/ or terragrunt/accounts/: {module}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_account_uses_legacy_for_terraform() {
        assert_eq!(
            module_account(Utf8Path::new("terraform/foo/bar")),
            Account::Legacy
        );
    }

    #[test]
    fn module_account_uses_legacy_for_legacy_terragrunt() {
        assert_eq!(
            module_account(Utf8Path::new("terragrunt/accounts/legacy/prod")),
            Account::Legacy
        );
    }

    #[test]
    fn module_account_uses_sso_for_non_legacy_terragrunt() {
        assert_eq!(
            module_account(Utf8Path::new("terragrunt/accounts/prod-eastus/app")),
            Account::Sso("prod-eastus".to_string())
        );
    }
}
