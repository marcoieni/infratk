use std::collections::BTreeMap;

use secrecy::SecretString;

use crate::{cmd::Cmd, config::Config, git};

/// Returns a map of environment variables that you need to use to authenticate with the account.
#[must_use]
pub fn login(account_dir: &str, config: &Config) -> BTreeMap<String, SecretString> {
    match account_dir {
        "legacy" => legacy_login(config.op_legacy_item_id.as_deref()),
        _ => {
            sso_login(account_dir);
            BTreeMap::new()
        }
    }
}

/// Returns a map of environment variables that can be used to authenticate with the legacy account.
pub fn legacy_login(op_legacy_item_id: Option<&str>) -> BTreeMap<String, SecretString> {
    let repo = git::repo();
    let git_root = git::git_root(&repo);
    let mut env_vars = BTreeMap::new();
    let mut cred_cmd = Cmd::new("python3", ["./aws-creds.py"]);
    cred_cmd
        .hide_command()
        .hide_stdout()
        .with_current_dir(git_root);
    if let Some(op_legacy_item_id) = op_legacy_item_id {
        let mut totp_cmd = Cmd::new("op", ["item", "get", op_legacy_item_id, "--otp"]);
        totp_cmd.hide_command().hide_stdout();
        let totp_code_output = totp_cmd.run();
        assert!(totp_code_output.status().success());
        let totp_code = totp_code_output.stdout().trim().to_string();
        cred_cmd.with_env_vars([("TOTP_CODE".to_string(), totp_code.into())].into());
    }
    let outcome = cred_cmd.run();
    assert!(
        outcome.status().success(),
        "failed to login to legacy account"
    );
    for line in outcome.stdout().lines() {
        let Some(line) = line.trim().strip_prefix("export ") else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        env_vars.insert(key.to_string(), SecretString::new(value.into()));
    }
    env_vars
}

fn sso_login_cmd(account_dir: &str) -> Cmd {
    let profile = sso_profile(account_dir);
    Cmd::new("aws", ["sso", "login", "--profile", &profile])
}

fn sso_logout_cmd() -> Cmd {
    Cmd::new("aws", ["sso", "logout"])
}

pub fn sso_login(account_dir: &str) {
    let output = sso_login_cmd(account_dir).run();
    assert!(output.status().success());
}

pub fn sso_login_quiet(account_dir: &str) {
    let mut cmd = sso_login_cmd(account_dir);
    cmd.hide_command().hide_stdout();
    let output = cmd.run();
    assert!(output.status().success());
}

pub fn sso_logout() {
    let output = sso_logout_cmd().run();
    assert!(output.status().success());
}

pub fn sso_profile(account_dir: &str) -> String {
    assert_ne!(
        account_dir, "legacy",
        "can't login to legacy account with sso"
    );
    let account = match account_dir {
        "root" => "rust-root",
        account_dir => account_dir,
    };
    account.to_string()
}
