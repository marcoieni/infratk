use camino::{Utf8Path, Utf8PathBuf};
use tracing::debug;

use crate::{
    args::UpgradeArgs,
    aws, clipboard,
    cmd_runner::{CmdRunner, PlanOutcome},
    config::Config,
    dir,
    envirnoment::assert_aws_env_is_not_set,
    git,
    graph::ModulesGraph,
    grouped_dirs::GroupedDirs,
    pretty_format, select,
};

pub fn upgrade(args: UpgradeArgs, config: &Config) {
    let repo = git::repo();
    assert_aws_env_is_not_set();

    let plan_outcome = if args.git {
        let changed_files = repo
            .changes_except_typechanges()
            .unwrap()
            .iter()
            .map(Utf8PathBuf::from)
            .map(|p| dir::get_stripped_parent(&p))
            .collect::<Vec<_>>();
        let graph = ModulesGraph::new(None);
        let dependent_modules = graph.get_dependent_modules_containing_lockfile(&changed_files);
        println!("ℹ️ Upgrading dependent modules of {changed_files:?}: {dependent_modules:?}");
        let grouped_dirs = GroupedDirs::new(dependent_modules);
        grouped_dirs.upgrade_all(config)
    } else {
        let git_root = git::git_root(&repo);
        let tg_accounts = git_root.join("terragrunt").join("accounts");
        let accounts = list_directories_at_path(&tg_accounts);
        let selected_accounts = select::select_accounts(accounts);
        println!("Selected accounts: {:?}", selected_accounts);
        upgrade_accounts(selected_accounts, config)
    };
    let output_str = pretty_format::format_output(plan_outcome);
    println!("{output_str}");
    if args.clipboard {
        clipboard::copy_to_clipboard(output_str);
    }
}

fn upgrade_accounts(
    accounts: Vec<Utf8PathBuf>,
    config: &Config,
) -> Vec<(Utf8PathBuf, PlanOutcome)> {
    let mut outcome = vec![];
    for account in accounts {
        // logout before login, to avoid issues with multiple profiles
        aws::sso_logout();
        let env_vars = aws::login(account.file_name().unwrap(), config);
        let cmd_runner = CmdRunner::new(env_vars);
        let states = list_directories_at_path(&account);
        let selected_states = select::select_states(states);
        println!("Selected states: {:?}", selected_states);
        for state in selected_states {
            // Update lockfile
            cmd_runner.terragrunt_init_upgrade(&state);
            let plan_outcome = cmd_runner.terragrunt_plan(&state);
            outcome.push((state.to_path_buf(), plan_outcome));
        }
    }
    outcome
}

fn list_directories_at_path(path: &Utf8Path) -> Vec<Utf8PathBuf> {
    debug!("Listing directories at path: {:?}", path);
    let mut children_dirs = vec![];
    let dir = path.read_dir().unwrap();
    for entry in dir {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            let utf8_path = Utf8PathBuf::from_path_buf(path).unwrap();
            children_dirs.push(utf8_path);
        }
    }
    children_dirs
}
