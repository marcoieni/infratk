use std::collections::BTreeSet;

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use inquire::Select;

use crate::{
    aws,
    command::legacy_login::login_to_legacy_aws_account,
    config::Config,
    dir::{self, current_dir_is_simpleinfra},
};

const LEGACY_AWS_ENV_VARS: [&str; 4] = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_SECURITY_TOKEN",
];

pub fn cd(config: &Config) {
    assert!(current_dir_is_simpleinfra());
    let modules = list_modules();
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

fn list_modules() -> Vec<Utf8PathBuf> {
    let mut modules = BTreeSet::new();
    let walker = ignore::WalkBuilder::new(dir::current_dir())
        .hidden(false)
        .build();

    for entry in walker {
        let entry = entry.expect("invalid entry");
        let file_type = entry.file_type().expect("unknown file type");
        if file_type.is_dir() {
            continue;
        }

        let path = Utf8PathBuf::from_path_buf(entry.path().to_path_buf()).unwrap();
        if is_path_ignored(&path) {
            continue;
        }

        let parent = dir::get_stripped_parent(&path);
        if (is_terragrunt_module_file(&path) && is_terragrunt_module_dir(&parent))
            || (is_terraform_module_file(&path) && is_terraform_module_dir(&parent))
        {
            modules.insert(parent);
        }
    }

    assert!(
        !modules.is_empty(),
        "no terragrunt/terraform modules found in this repository"
    );
    modules.into_iter().collect()
}

fn is_path_ignored(path: &Utf8Path) -> bool {
    path.components().any(|c| {
        c == Utf8Component::Normal(".git")
            || c == Utf8Component::Normal(".terraform")
            || c == Utf8Component::Normal(".terragrunt-cache")
    })
}

fn is_terragrunt_module_file(path: &Utf8Path) -> bool {
    path.file_name() == Some("terragrunt.hcl")
}

fn is_terraform_module_file(path: &Utf8Path) -> bool {
    path.extension() == Some("tf")
}

fn is_terraform_module_dir(path: &Utf8Path) -> bool {
    path.components().next() == Some(Utf8Component::Normal("terraform"))
}

fn is_terragrunt_module_dir(path: &Utf8Path) -> bool {
    let mut components = path.components();
    let first = components.next();
    let second = components.next();
    let third = components.next();
    first == Some(Utf8Component::Normal("terragrunt"))
        && second == Some(Utf8Component::Normal("accounts"))
        && third.is_some()
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
