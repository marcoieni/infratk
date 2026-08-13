use camino::{Utf8Path, Utf8PathBuf};

use crate::{
    args::PlanPr, clipboard, cmd::Cmd, cmd_runner::PlanOutcome, config::Config,
    dir::current_dir_is_simpleinfra, git::assert_current_branch_is_same_as_pr,
    grouped_dirs::GroupedDirs, pretty_format, LOCKFILE,
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
    GroupedDirs::new(directories).plan_all(config)
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
