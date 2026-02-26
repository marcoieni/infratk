mod args;
mod aws;
mod clipboard;
mod cmd;
mod cmd_runner;
mod command;
mod config;
mod dir;
mod envirnoment;
mod git;
mod graph;
mod grouped_dirs;
mod log;
mod pretty_format;
mod provider;
mod select;

use args::CliArgs;
use clap::Parser as _;

const LOCKFILE: &str = ".terraform.lock.hcl";

#[tokio::main]
async fn main() {
    log::init(true);
    let args = CliArgs::parse();
    let config = config::parse_config().unwrap();
    match args.command {
        args::Command::Upgrade(args) => command::upgrade::upgrade(&args, &config),
        args::Command::PlanPr(args) => command::plan_pr::plan_pr(&args, &config),
        args::Command::UpgradeProvider => {
            command::upgrade_provider::upgrade_provider(&config).await;
        }
        args::Command::Config => command::config_cmd::create_default_config(),
        args::Command::LegacyLogin => command::legacy_login::login_to_legacy_aws_account(&config),
        args::Command::Cd => command::cd_cmd::cd(&config),
        args::Command::Graph(args) => command::graph_cmd::print_graph(args).await,
    }
}
