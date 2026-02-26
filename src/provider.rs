use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use semver::Version;

use crate::{
    command::upgrade_provider::{ProviderVersions, Providers},
    dir, LOCKFILE,
};

async fn get_latest_version(provider: &str) -> anyhow::Result<Version> {
    #[derive(serde::Deserialize)]
    struct ProviderJson {
        version: String,
        published_at: String,
    }

    let url = format!("https://registry.terraform.io/v1/providers/{provider}");
    let response: ProviderJson = reqwest::get(&url).await?.json().await?;

    let version = semver::Version::parse(&response.version)?;
    println!("- {} - `{provider}`: {version} \t", response.published_at);
    Ok(version)
}

pub async fn outdated_providers(providers: Providers) -> anyhow::Result<Providers> {
    let mut outdated = BTreeMap::new();
    println!("latest providers versions:");
    for (provider_name, provider_versions) in providers.providers {
        let latest_version = get_latest_version(&provider_name).await?;
        let mut outdated_versions = BTreeMap::new();
        for (version, lockfiles) in provider_versions.versions {
            if version != latest_version {
                outdated_versions.insert(version.clone(), lockfiles);
            }
        }
        if !outdated_versions.is_empty() {
            outdated.insert(
                provider_name,
                ProviderVersions {
                    versions: outdated_versions,
                },
            );
        }
    }
    Ok(Providers {
        providers: outdated,
    })
}

pub fn get_all_lockfiles() -> Vec<Utf8PathBuf> {
    let mut lockfiles = vec![];
    let current_dir = dir::current_dir();
    let walker = ignore::WalkBuilder::new(current_dir)
        // Read hidden files
        .hidden(false)
        .build();
    for entry in walker {
        let entry = entry.expect("invalid entry");
        let file_type = entry.file_type().expect("unknown file type");
        if !file_type.is_dir() && entry.file_name() == LOCKFILE {
            let path = entry.path().to_path_buf();
            let utf8path = Utf8PathBuf::from_path_buf(path).unwrap();
            lockfiles.push(utf8path);
        }
    }
    lockfiles
}

/// Get all providers from all lockfiles.
/// The result is a map where the key is the provider name and the value is the
/// list of lockfiles that use that provider.
pub fn get_all_providers(lockfiles: &[Utf8PathBuf]) -> Providers {
    let mut providers = BTreeMap::new();
    for lockfile in lockfiles {
        let content = std::fs::read_to_string(lockfile).expect("could not read lockfile");
        let mut lines = content.lines();
        while let Some(line) = lines.next() {
            if line.starts_with("provider") {
                let provider_name = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .trim_matches('"')
                    .strip_prefix("registry.terraform.io/")
                    .expect("invalid provider name")
                    .to_string();
                if let Some(version_line) = lines.next() {
                    let version = version_line
                        .split_whitespace()
                        .nth(2)
                        .unwrap()
                        .trim_matches('"');
                    let version = Version::parse(version).unwrap();
                    providers
                        .entry(provider_name)
                        .or_insert_with(|| ProviderVersions {
                            versions: BTreeMap::new(),
                        })
                        .versions
                        .entry(version)
                        .or_insert_with(Vec::new)
                        .push(lockfile.clone());
                }
            }
        }
    }
    Providers { providers }
}
