use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use camino::{Utf8Path, Utf8PathBuf};
use tracing::{debug, warn};

use petgraph::{graph::NodeIndex, Direction, Graph};

use crate::{dir, LOCKFILE};

/// A graph of terragrunt and terraform modules.
pub struct ModulesGraph {
    pub graph: Graph<Utf8PathBuf, i32>,
}

impl ModulesGraph {
    pub fn new(outdated_packages: Option<&BTreeSet<Utf8PathBuf>>) -> Self {
        Self::from_files(get_all_tf_and_hcl_files(), outdated_packages)
    }

    fn from_files(
        files: Vec<Utf8PathBuf>,
        outdated_packages: Option<&BTreeSet<Utf8PathBuf>>,
    ) -> Self {
        let mut graph: Graph<Utf8PathBuf, i32> = Graph::new();
        // Collection of `file` - `graph index`.
        let mut indices = HashMap::<Utf8PathBuf, NodeIndex>::new();
        for f in files {
            let f_parent = dir::get_stripped_parent(&f);
            let node_index = indices
                .get(&f_parent)
                .cloned()
                .unwrap_or_else(|| add_node(&mut graph, f_parent, &mut indices, outdated_packages));
            let dependencies = get_dependencies(&f);
            for d in dependencies {
                let d_index = indices
                    .get(&d)
                    .cloned()
                    .unwrap_or_else(|| add_node(&mut graph, d, &mut indices, outdated_packages));

                graph.update_edge(node_index, d_index, 0);
            }
        }
        Self { graph }
    }

    pub fn get_dependent_modules_containing_lockfile<T>(&self, modules: &[T]) -> Vec<Utf8PathBuf>
    where
        T: AsRef<Utf8Path>,
    {
        self.get_dependent_modules(modules)
            .iter()
            .filter(|m| m.join(LOCKFILE).exists())
            .cloned()
            .collect()
    }

    /// Find deployable root modules affected by a set of changed files.
    ///
    /// A change to a reusable module also selects every root module depending
    /// on it. Account-level Terragrunt configuration selects all roots in that
    /// account, while repository-level Terragrunt configuration selects roots
    /// in every account.
    pub fn get_affected_modules_containing_lockfile<T>(
        &self,
        changed_files: &[T],
    ) -> Vec<Utf8PathBuf>
    where
        T: AsRef<Utf8Path>,
    {
        self.get_affected_modules(changed_files)
            .into_iter()
            .filter(|module| module.join(LOCKFILE).exists())
            .collect()
    }

    fn get_affected_modules<T>(&self, changed_files: &[T]) -> Vec<Utf8PathBuf>
    where
        T: AsRef<Utf8Path>,
    {
        let modules = self.graph.node_weights().cloned().collect::<BTreeSet<_>>();
        let mut directly_changed = BTreeSet::new();

        for file in changed_files {
            let file = file.as_ref();
            if is_global_terragrunt_file(file) {
                directly_changed.extend(
                    modules
                        .iter()
                        .filter(|module| module.starts_with("terragrunt/accounts"))
                        .cloned(),
                );
                continue;
            }

            if let Some(account) = account_for_account_level_file(file) {
                let account_dir = Utf8Path::new("terragrunt/accounts").join(account);
                directly_changed.extend(
                    modules
                        .iter()
                        .filter(|module| module.starts_with(&account_dir))
                        .cloned(),
                );
                continue;
            }

            let closest_module = modules
                .iter()
                .filter(|module| file.starts_with(module))
                .max_by_key(|module| module.components().count());
            if let Some(module) = closest_module {
                directly_changed.insert(module.clone());
            } else {
                warn!("No Terraform or Terragrunt module found for changed file {file}");
            }
        }

        let directly_changed = directly_changed.into_iter().collect::<Vec<_>>();
        let affected = self.get_dependent_modules(&directly_changed);
        self.order_modules_prerequisites_first(&affected)
    }

    /// Order modules so every selected dependency precedes its dependents.
    fn order_modules_prerequisites_first(&self, modules: &[Utf8PathBuf]) -> Vec<Utf8PathBuf> {
        let indices = self.module_indices();
        let mut roots = modules
            .iter()
            .map(|module| indices[module.as_path()])
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| self.graph[*left].cmp(&self.graph[*right]));
        let selected = roots.iter().copied().collect::<HashSet<_>>();
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        let mut ordered = Vec::with_capacity(selected.len());

        for index in roots {
            self.visit_dependencies_first(
                index,
                &selected,
                &mut visiting,
                &mut visited,
                &mut ordered,
            );
        }

        ordered
    }

    fn visit_dependencies_first(
        &self,
        index: NodeIndex,
        selected: &HashSet<NodeIndex>,
        visiting: &mut HashSet<NodeIndex>,
        visited: &mut HashSet<NodeIndex>,
        ordered: &mut Vec<Utf8PathBuf>,
    ) {
        if visited.contains(&index) {
            return;
        }
        assert!(
            visiting.insert(index),
            "dependency cycle detected at {}",
            self.graph[index]
        );

        let mut dependencies = self
            .graph
            .neighbors_directed(index, Direction::Outgoing)
            .filter(|dependency| selected.contains(dependency))
            .collect::<Vec<_>>();
        dependencies.sort_by(|left, right| self.graph[*left].cmp(&self.graph[*right]));
        for dependency in dependencies {
            self.visit_dependencies_first(dependency, selected, visiting, visited, ordered);
        }

        visiting.remove(&index);
        visited.insert(index);
        ordered.push(self.graph[index].clone());
    }

    pub fn get_dependent_modules<T>(&self, modules: &[T]) -> Vec<Utf8PathBuf>
    where
        T: AsRef<Utf8Path>,
    {
        let indices = self.module_indices();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();

        for module in modules {
            let module = module.as_ref();
            let index = *indices
                .get(module)
                .unwrap_or_else(|| panic!("module not found in graph: {module}"));
            if visited.insert(index) {
                queue.push_back(index);
            }
        }

        let mut dependent_modules = Vec::new();
        while let Some(index) = queue.pop_front() {
            debug!("Found dependent module: {:?}", self.graph[index]);
            dependent_modules.push(self.graph[index].clone());

            let mut dependents = self
                .graph
                .neighbors_directed(index, Direction::Incoming)
                .collect::<Vec<_>>();
            dependents.sort_by(|left, right| self.graph[*left].cmp(&self.graph[*right]));
            for dependent in dependents {
                if visited.insert(dependent) {
                    queue.push_back(dependent);
                }
            }
        }

        dependent_modules
    }

    fn module_indices(&self) -> HashMap<&Utf8Path, NodeIndex> {
        self.graph
            .node_indices()
            .map(|index| (self.graph[index].as_path(), index))
            .collect()
    }
}

fn is_global_terragrunt_file(file: &Utf8Path) -> bool {
    file.starts_with("terragrunt")
        && !file.starts_with("terragrunt/accounts")
        && !file.starts_with("terragrunt/modules")
}

fn account_for_account_level_file(file: &Utf8Path) -> Option<&str> {
    let relative = file.strip_prefix("terragrunt/accounts").ok()?;
    let mut components = relative.components();
    let account = components.next()?.as_str();

    // A file directly in the account directory affects every state in it. A
    // file below the next component belongs to one specific state instead.
    (components.count() == 1).then_some(account)
}

fn add_node(
    graph: &mut Graph<Utf8PathBuf, i32>,
    dir: Utf8PathBuf,
    indices: &mut HashMap<Utf8PathBuf, NodeIndex>,
    outdated_packages: Option<&BTreeSet<Utf8PathBuf>>,
) -> NodeIndex {
    let label = if let Some(outdated_packages) = outdated_packages {
        // add an emoji to the path just for the graph visualization.
        if outdated_packages.contains(&dir) {
            // the module isn't up-to-date and it needs to be updated.
            dir.join(" ❌")
        } else if dir.join(LOCKFILE).exists() {
            // the module isn't in the outdated packages and it contains a lockfile, so it's up-to-date
            dir.join(" ✅")
        } else {
            // The module doesn't contain a lockfile, so we don't need to update it.
            dir.clone()
        }
    } else {
        dir.clone()
    };
    debug!("Adding node: {:?}", label);
    let node_index = graph.add_node(label.clone());
    indices.insert(dir, node_index);
    node_index
}

/// Get the dependencies of a file
/// Dependencies are anything in the file like `source = "path"` or `config_path = "path"`.
fn get_dependencies(file: &Utf8Path) -> Vec<Utf8PathBuf> {
    let content = std::fs::read_to_string(file).expect("could not read file");
    let mut dependencies = vec![];
    for line in content.lines() {
        if let Some(dependency) = get_dependency_from_line(line) {
            let module_path = file.parent().unwrap().join(dependency);
            let relative_path = get_relative_path(&module_path);
            debug!("found dependency {:?} from line {line}", relative_path);
            dependencies.push(relative_path);
        }
    }
    dependencies
}

pub fn get_all_modules() -> Vec<Utf8PathBuf> {
    // Use BTreeSet for alphabetical order.
    let mut dirs = BTreeSet::new();
    let current_dir = dir::current_dir();
    let walker = ignore::WalkBuilder::new(current_dir).build();

    for entry in walker {
        let entry = entry.expect("invalid entry");
        let file_type = entry.file_type().expect("unknown file type");
        if !file_type.is_dir() {
            let parent = entry.path().parent().expect("file without parent");
            let utf8_parent = Utf8Path::from_path(parent).expect("invalid utf-8 path");
            let stripped_parent = dir::strip_current_dir(utf8_parent);

            // Once a module directory was recorded, skip checking more files in it.
            if dirs.contains(&stripped_parent) {
                continue;
            }

            if entry.path().extension() == Some("tf".as_ref())
                || entry.path().extension() == Some("hcl".as_ref())
            {
                dirs.insert(stripped_parent);
            }
        }
    }

    assert!(
        !dirs.is_empty(),
        "no terragrunt/terraform modules found in this repository"
    );
    dirs.into_iter().collect()
}

/// Get all the files that might contain a dependency
pub fn get_all_tf_and_hcl_files() -> Vec<Utf8PathBuf> {
    let mut files = vec![];
    let current_dir = dir::current_dir();
    let walker = ignore::WalkBuilder::new(current_dir)
        // Read hidden files
        .hidden(false)
        .build();

    for entry in walker {
        let entry = entry.expect("invalid entry");
        let file_type = entry.file_type().expect("unknown file type");
        if !file_type.is_dir()
            && (entry.path().extension() == Some("tf".as_ref())
                || entry.path().extension() == Some("hcl".as_ref()))
        {
            let path = entry.path().to_path_buf();
            let utf8path = Utf8PathBuf::from_path_buf(path).unwrap();
            files.push(utf8path);
        }
    }
    files
}

fn get_dependency_from_line(line: &str) -> Option<&str> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let first_token = *tokens.first()?;
    if first_token != "source" && first_token != "config_path" {
        return None;
    }
    let second_token = *tokens.get(1)?;
    if second_token != "=" {
        return None;
    }
    let third_token = tokens[2].trim_matches('"');
    let dependency = third_token
        .trim_start_matches("git::")
        .split('?')
        .next()
        .unwrap_or(third_token);
    if !dependency.starts_with(".") {
        // it's not a directory. E.g. it's `source  = "hashicorp/aws"`.
        return None;
    }

    Some(dependency)
}

fn get_relative_path(path: &Utf8Path) -> Utf8PathBuf {
    // canonicalize to convert `a/b/../c` to `a/c`
    let canonicalized = match path.canonicalize_utf8() {
        Ok(c) => c,
        Err(err) => {
            warn!("Could not canonicalize path {path}: {err:?}");
            path.to_path_buf()
        }
    };
    dir::strip_current_dir(&canonicalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino_tempfile::{NamedUtf8TempFile, Utf8TempDir};

    #[test]
    fn dependencies_are_read() {
        let file = NamedUtf8TempFile::new().unwrap();
        let content = r#"
                        source = "../aaaa"
                "#;
        fs_err::write(file.path(), content).unwrap();
        let dependencies = get_dependencies(file.path());
        assert_eq!(dependencies.len(), 1);
    }

    #[test]
    fn reusable_module_change_selects_dependent_state() {
        let temp = Utf8TempDir::new().unwrap();
        let reusable_module = temp.path().join("terragrunt/modules/runtime");
        let other_module = temp.path().join("terragrunt/modules/database");
        let dependent_state = temp
            .path()
            .join("terragrunt/accounts/production/billing-api");
        let same_named_state = temp.path().join("terragrunt/accounts/production/runtime");

        for directory in [
            &reusable_module,
            &other_module,
            &dependent_state,
            &same_named_state,
        ] {
            fs_err::create_dir_all(directory).unwrap();
        }

        let reusable_module_file = reusable_module.join("main.tf");
        let other_module_file = other_module.join("main.tf");
        let dependent_state_file = dependent_state.join("terragrunt.hcl");
        let same_named_state_file = same_named_state.join("terragrunt.hcl");
        fs_err::write(&reusable_module_file, "").unwrap();
        fs_err::write(&other_module_file, "").unwrap();
        fs_err::write(
            &dependent_state_file,
            r#"
terraform {
    source = "../../../modules//runtime"
}
"#,
        )
        .unwrap();
        fs_err::write(
            &same_named_state_file,
            r#"
terraform {
    source = "../../../modules//database"
}
"#,
        )
        .unwrap();

        let graph = ModulesGraph::from_files(
            vec![
                reusable_module_file.clone(),
                other_module_file,
                dependent_state_file,
                same_named_state_file,
            ],
            None,
        );

        assert_eq!(
            graph.get_affected_modules(&[reusable_module_file]),
            vec![reusable_module, dependent_state]
        );
    }

    #[test]
    fn affected_modules_are_ordered_prerequisites_first() {
        let mut graph = Graph::new();
        let reusable = graph.add_node(Utf8PathBuf::from("terragrunt/modules/network"));
        let prerequisite =
            graph.add_node(Utf8PathBuf::from("terragrunt/accounts/production/z-vpc"));
        let middle = graph.add_node(Utf8PathBuf::from(
            "terragrunt/accounts/production/b-cluster",
        ));
        let dependent = graph.add_node(Utf8PathBuf::from(
            "terragrunt/accounts/production/a-service",
        ));
        graph.add_edge(prerequisite, reusable, 0);
        graph.add_edge(middle, prerequisite, 0);
        graph.add_edge(dependent, middle, 0);
        let graph = ModulesGraph { graph };

        assert_eq!(
            graph.get_affected_modules(&["terragrunt/modules/network/main.tf"]),
            vec![
                Utf8PathBuf::from("terragrunt/modules/network"),
                Utf8PathBuf::from("terragrunt/accounts/production/z-vpc"),
                Utf8PathBuf::from("terragrunt/accounts/production/b-cluster"),
                Utf8PathBuf::from("terragrunt/accounts/production/a-service"),
            ]
        );
    }

    #[test]
    fn account_config_change_selects_all_account_states() {
        let mut graph = Graph::new();
        graph.add_node(Utf8PathBuf::from(
            "terragrunt/accounts/production/service-a",
        ));
        graph.add_node(Utf8PathBuf::from(
            "terragrunt/accounts/production/service-b",
        ));
        graph.add_node(Utf8PathBuf::from("terragrunt/accounts/staging/service-a"));
        let graph = ModulesGraph { graph };

        assert_eq!(
            graph.get_affected_modules(&["terragrunt/accounts/production/account.json"]),
            vec![
                Utf8PathBuf::from("terragrunt/accounts/production/service-a"),
                Utf8PathBuf::from("terragrunt/accounts/production/service-b"),
            ]
        );
    }

    #[test]
    fn multi_source_traversal_deduplicates_shared_dependents() {
        let mut graph = Graph::new();
        let module_a = graph.add_node(Utf8PathBuf::from("terragrunt/modules/a"));
        let module_b = graph.add_node(Utf8PathBuf::from("terragrunt/modules/b"));
        let state = graph.add_node(Utf8PathBuf::from("terragrunt/accounts/production/service"));
        graph.add_edge(state, module_a, 0);
        graph.add_edge(state, module_b, 0);
        let graph = ModulesGraph { graph };

        assert_eq!(
            graph.get_dependent_modules(&[
                Utf8PathBuf::from("terragrunt/modules/a"),
                Utf8PathBuf::from("terragrunt/modules/b"),
            ]),
            vec![
                Utf8PathBuf::from("terragrunt/modules/a"),
                Utf8PathBuf::from("terragrunt/modules/b"),
                Utf8PathBuf::from("terragrunt/accounts/production/service"),
            ]
        );
    }
}
