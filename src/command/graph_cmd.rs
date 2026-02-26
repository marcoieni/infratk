use std::collections::{BTreeMap, BTreeSet};

use camino::Utf8PathBuf;
use petgraph::dot::{self, Dot};
use semver::Version;

use crate::{args::GraphArgs, clipboard, dir, graph::ModulesGraph, provider};

pub async fn print_graph(args: GraphArgs) {
    assert!(dir::current_dir_is_simpleinfra());

    let min_versions = args.min_versions();
    let outdated_packages = args
        .outdated
        .then(|| get_packages_with_outdated_providers(&min_versions));
    let outdated_packages = match outdated_packages {
        Some(outdated_packages) => Some(outdated_packages.await),
        None => None,
    };

    let graph = ModulesGraph::new(outdated_packages.as_ref());

    // Get `graphviz` format
    let output_str = format!(
        "{:?}",
        Dot::with_config(&graph.graph, &[dot::Config::EdgeNoLabel])
    );
    println!("{output_str:?}");

    if args.clipboard {
        clipboard::copy_to_clipboard(&output_str);
    }
}

async fn get_packages_with_outdated_providers(
    min_versions: &BTreeMap<String, Version>,
) -> BTreeSet<Utf8PathBuf> {
    let lockfiles = provider::get_all_lockfiles();
    let providers = provider::get_all_providers(&lockfiles);
    let outdated_providers = provider::outdated_providers(providers).await.unwrap();

    let mut outdated_packages = BTreeSet::new();
    for (provider, versions) in outdated_providers.providers {
        for (version, lockfiles) in versions.versions {
            if let Some(min_ver) = min_versions.get(&provider) {
                if &version >= min_ver {
                    continue;
                }
            }
            let parents = lockfiles
                .iter()
                .map(dir::get_stripped_parent)
                .collect::<Vec<_>>();
            outdated_packages.extend(parents);
        }
    }
    outdated_packages
}
