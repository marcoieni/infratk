use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};

use crate::{
    args::PlanPr,
    aws, clipboard,
    cmd::Cmd,
    cmd_runner::{CmdRunner, PlanOutcome},
    config::Config,
    dir::current_dir_is_simpleinfra,
    git::assert_current_branch_is_same_as_pr,
    grouped_dirs::GroupedDirs,
    pretty_format, LOCKFILE,
};

pub fn plan_pr(args: &PlanPr, config: &Config) {
    assert!(current_dir_is_simpleinfra());
    assert_current_branch_is_same_as_pr(&args.pr);
    let files_changed = get_files_changes(&args.pr);
    println!("Files changed in PR: {files_changed:?}");
    let lock_files = get_lock_files(&files_changed);
    println!("Lock files changed in PR: {lock_files:?}");
    let directories: Vec<&Utf8Path> = lock_files
        .iter()
        .map(|file| file.parent().unwrap())
        .collect();
    let output = plan_directories(&directories, config);
    let output_str = pretty_format::format_output(output);
    println!("{output_str}");
    if args.clipboard {
        clipboard::copy_to_clipboard(&output_str);
    }
}

fn plan_directories(directories: &[&Utf8Path], config: &Config) -> Vec<(Utf8PathBuf, PlanOutcome)> {
    let grouped_dirs = GroupedDirs::new(directories);

    let mut output: Vec<(Utf8PathBuf, PlanOutcome)> = vec![];
    if grouped_dirs.contains_legacy_account() {
        let legacy_tg_dirs = grouped_dirs.legacy_terragrunt_dirs();
        let o = plan_legacy_dirs(grouped_dirs.terraform_dirs(), legacy_tg_dirs, config);
        output.extend(o);
    }

    let sso_terragrunt_dirs = grouped_dirs.sso_terragrunt_dirs();
    let o = plan_terragrunt_with_sso(&sso_terragrunt_dirs);
    output.extend(o);

    output
}

fn plan_legacy_dirs<T, U>(
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

    let mut output = vec![];
    for d in terraform_dirs {
        let d = d.as_ref();
        let o = cmd_runner.terraform_plan(d);
        output.push((d.to_path_buf(), o));
    }
    for d in terragrunt_dirs {
        let d = d.as_ref();
        let o = cmd_runner.terragrunt_plan(d);
        output.push((d.to_path_buf(), o));
    }
    output
}

fn plan_terragrunt_with_sso<T>(
    terragrunt_sso_dirs: &BTreeMap<&str, Vec<T>>,
) -> Vec<(Utf8PathBuf, PlanOutcome)>
where
    T: AsRef<Utf8Path>,
{
    let terragrunt_sso_dirs = terragrunt_sso_dirs
        .iter()
        .map(|(k, v)| (*k, v.iter().map(|d| d.as_ref()).collect::<Vec<_>>()))
        .collect::<BTreeMap<_, _>>();
    let mut outcome = vec![];
    for (account, dirs) in terragrunt_sso_dirs {
        aws::sso_logout();
        aws::sso_login(account);
        for d in dirs {
            let plan_outcome = CmdRunner::new(BTreeMap::new()).terragrunt_plan(d);
            outcome.push((d.to_path_buf(), plan_outcome));
        }
    }
    outcome
}

fn get_files_changes(pr: &str) -> Vec<Utf8PathBuf> {
    Cmd::new("gh", ["pr", "diff", pr, "--name-only"])
        .hide_stdout()
        .run()
        .stdout()
        .lines()
        .map(Utf8PathBuf::from)
        .collect()
}

fn get_lock_files(files: &[Utf8PathBuf]) -> Vec<Utf8PathBuf> {
    files
        .iter()
        .filter(|file| file.file_name() == Some(LOCKFILE))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_files_are_filtered() {
        let files = vec![
            Utf8PathBuf::from("main.tf"),
            Utf8PathBuf::from("module1/main.tf"),
            Utf8PathBuf::from("module1/.terraform.lock.hcl"),
            Utf8PathBuf::from("module2/.terraform.lock.hcl"),
        ];
        let lock_files = get_lock_files(&files);
        assert_eq!(
            lock_files,
            vec![
                Utf8PathBuf::from("module1/.terraform.lock.hcl"),
                Utf8PathBuf::from("module2/.terraform.lock.hcl"),
            ]
        );
    }
}
