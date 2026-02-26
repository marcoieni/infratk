use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use inquire::{list_option::ListOption, validator::Validation, MultiSelect};
use semver::Version;
use std::fmt;

use crate::{
    config::Config,
    dir::{self, current_dir_is_simpleinfra},
    envirnoment::assert_aws_env_is_not_set,
    grouped_dirs, pretty_format,
    provider::{self, get_all_lockfiles, get_all_providers},
};

pub async fn upgrade_provider(config: &Config) {
    assert!(current_dir_is_simpleinfra());
    assert_aws_env_is_not_set();
    let lockfiles = get_all_lockfiles();
    let providers = get_all_providers(&lockfiles);
    let outdated_providers = provider::outdated_providers(providers).await.unwrap();
    println!("\nOutdated providers: {outdated_providers}");
    let providers_list = outdated_providers.providers.keys().cloned().collect();
    let selected_providers = select_providers(providers_list);

    update_lockfiles(&outdated_providers, &selected_providers, config);
}

fn update_lockfiles(providers: &Providers, selected_providers: &[String], config: &Config) {
    // Filter out the providers that were not selected
    let filtered_providers = providers
        .providers
        .iter()
        .filter(|(k, _)| selected_providers.contains(k))
        .collect::<BTreeMap<_, _>>();

    let all_dirs: Vec<Utf8PathBuf> = filtered_providers
        .values()
        .flat_map(|v| v.versions.values())
        .flat_map(|paths| get_parents(paths))
        .collect();

    let grouped_dirs = grouped_dirs::GroupedDirs::new(&all_dirs);

    let outcome = grouped_dirs.upgrade_all(config);
    pretty_format::format_output(outcome);
}

fn get_parents(paths: &[Utf8PathBuf]) -> Vec<Utf8PathBuf> {
    paths
        .iter()
        .map(|p| p.parent().unwrap().to_path_buf())
        .collect()
}

pub fn select_providers(providers: Vec<String>) -> Vec<String> {
    let selected = MultiSelect::new("Select one or more providers:", providers)
        .with_validator(|selected: &[ListOption<&String>]| {
            if selected.is_empty() {
                Ok(Validation::Invalid("Select one item!".into()))
            } else {
                Ok(Validation::Valid)
            }
        })
        .prompt()
        .unwrap_or_else(|e| panic!("failed to select providers: {e:?}"));

    selected.into_iter().collect()
}

#[derive(Debug, Clone)]
pub struct Providers {
    /// <provider name> -> <provider versions>
    pub providers: BTreeMap<String, ProviderVersions>,
}

impl fmt::Display for Providers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, versions) in &self.providers {
            writeln!(f, "- {name}:{versions}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ProviderVersions {
    /// <version> -> <lockfile where the version is present>
    pub versions: BTreeMap<Version, Vec<Utf8PathBuf>>,
}

impl fmt::Display for ProviderVersions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (version, lockfiles) in &self.versions {
            let lockfiles_fmt = lockfiles
                .iter()
                .map(|l| l.strip_prefix(dir::current_dir()).unwrap())
                .collect::<Vec<_>>();
            writeln!(f, "\n  - {version} -> {lockfiles_fmt:?}")?;
        }
        Ok(())
    }
}
