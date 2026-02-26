use std::collections::BTreeMap;

use semver::Version;

#[derive(clap::Parser, Debug)]
#[command(about, version, author)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Upgrade terragrunt states or Terraform modules.
    Upgrade(UpgradeArgs),
    /// Given a PR, run terragrunt/terraform plan on every module that changed.
    PlanPr(PlanPr),
    /// Select a provider and upgrade all lockfiles.
    UpgradeProvider,
    /// Create default configuration and print its path.
    /// If you are using 1Password, you can get an `ITEM_ID` by running
    /// `op item list`.
    Config,
    /// Print shell exports for the AWS legacy account credentials.
    /// To login, use as: `eval "$(infratk legacy-login)"`.
    #[command(visible_alias = "ll")]
    LegacyLogin,
    /// Get the graph of the terraform modules to see how they depend on each other.
    Graph(GraphArgs),
}

#[derive(clap::Parser, Debug)]
pub struct UpgradeArgs {
    /// If true, don't select accounts interactively but update the ones that are
    /// detected by git as untracked changes.
    #[arg(long)]
    pub git: bool,
    /// If true, copy the output to the clipboard.
    #[arg(long)]
    pub clipboard: bool,
}

#[derive(clap::Parser, Debug)]
pub struct PlanPr {
    /// PR Number OR URL OR Branch.
    pub pr: String,
    /// If true, copy the output to the clipboard.
    #[arg(long)]
    pub clipboard: bool,
}

#[derive(clap::Parser, Debug)]
pub struct GraphArgs {
    /// If true, copy the graphviz output to the clipboard.
    #[arg(long)]
    pub clipboard: bool,
    /// Check for outdated providers and show them in the graph.
    #[arg(long)]
    pub outdated: bool,
    /// Minimum versions of providers to not be considered outdated.
    /// E.g. `--min-versions hashicorp/aws=3.0.0,hashicorp/google=2.0.0`.
    #[arg(long)]
    min_versions: Vec<String>,
}

impl GraphArgs {
    pub fn min_versions(&self) -> BTreeMap<String, Version> {
        self.min_versions
            .iter()
            .map(|s| {
                let mut parts = s.split('=');
                let provider = parts.next().unwrap();
                let version = parts.next().unwrap();
                (provider.to_string(), Version::parse(version).unwrap())
            })
            .collect()
    }
}
