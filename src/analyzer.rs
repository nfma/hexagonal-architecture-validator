use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use cargo_metadata::{Edition, MetadataCommand, Package, Target};
use proc_macro2::Span;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Item, ItemMacro, ItemUse, Macro, Meta, Path as SynPath, Token, UseTree, Visibility,
};

use crate::model::{
    AnalysisDiagnostic, Dependency, DependencyGraph, Evidence, Module, OpaqueReexport,
};

const STANDARD_CRATES: [&str; 4] = ["alloc", "core", "proc_macro", "std"];
const STANDARD_PRELUDE_NAMES: &[&str] = &[
    "AsMut",
    "AsRef",
    "AsyncFn",
    "AsyncFnMut",
    "AsyncFnOnce",
    "Box",
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "DoubleEndedIterator",
    "Drop",
    "Eq",
    "Err",
    "ExactSizeIterator",
    "Extend",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "FromIterator",
    "Future",
    "Hash",
    "Into",
    "IntoFuture",
    "IntoIterator",
    "Iterator",
    "None",
    "Ok",
    "Option",
    "Ord",
    "PartialEq",
    "PartialOrd",
    "Result",
    "Send",
    "Sized",
    "Some",
    "String",
    "Sync",
    "ToOwned",
    "ToString",
    "TryFrom",
    "TryInto",
    "Unpin",
    "Vec",
    "align_of",
    "align_of_val",
    "alloc_error_handler",
    "assert",
    "assert_eq",
    "assert_ne",
    "bench",
    "cfg",
    "column",
    "compile_error",
    "concat",
    "dbg",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "derive",
    "drop",
    "env",
    "eprint",
    "eprintln",
    "file",
    "format",
    "format_args",
    "global_allocator",
    "include",
    "include_bytes",
    "include_str",
    "is_x86_feature_detected",
    "line",
    "matches",
    "module_path",
    "option_env",
    "panic",
    "print",
    "println",
    "size_of",
    "size_of_val",
    "stringify",
    "test",
    "test_case",
    "thread_local",
    "todo",
    "try",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

pub struct AnalysisOptions<'a> {
    pub root: &'a Path,
    pub manifest_path: Option<&'a Path>,
    pub strict: bool,
}

#[derive(Debug, Clone)]
struct TargetInfo {
    package: String,
    name: String,
    root_id: String,
    source: PathBuf,
    edition: Edition,
    workspace_aliases: BTreeMap<String, String>,
    external_aliases: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DependencyOrigin {
    Use,
    Path,
}

#[derive(Debug)]
struct RawDependency {
    source: String,
    target_root: String,
    current_segments: Vec<String>,
    segments: Vec<String>,
    leading_colon: bool,
    origin: DependencyOrigin,
    imported_name: Option<String>,
    public_import: bool,
    global_import: bool,
    evidence: Evidence,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct UseImport {
    segments: Vec<String>,
    exposed_name: String,
    leading_colon: bool,
}

struct DependencyPath {
    segments: Vec<String>,
    leading_colon: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct ImportTarget {
    module: String,
    exact_module: bool,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct ImportBinding {
    targets: BTreeSet<ImportTarget>,
    external: bool,
    local: bool,
    global: bool,
    opaque_routes: BTreeSet<(String, String)>,
}

impl ImportBinding {
    fn merge(&mut self, other: Self) {
        self.targets.extend(other.targets);
        self.external |= other.external;
        self.local |= other.local;
        self.global |= other.global;
        self.opaque_routes.extend(other.opaque_routes);
    }
}

struct Analyzer {
    workspace_root: PathBuf,
    strict: bool,
    targets: BTreeMap<String, TargetInfo>,
    graph: DependencyGraph,
    raw_dependencies: Vec<RawDependency>,
    module_paths: BTreeMap<(String, String), String>,
    module_locations: BTreeMap<String, (String, Vec<String>)>,
    module_items: BTreeMap<(String, String), BTreeSet<String>>,
    module_type_items: BTreeMap<(String, String), BTreeSet<String>>,
    module_sources: BTreeMap<String, PathBuf>,
    active_sources: BTreeSet<PathBuf>,
}

pub fn analyze(options: AnalysisOptions<'_>) -> anyhow::Result<DependencyGraph> {
    let root = options
        .root
        .canonicalize()
        .with_context(|| format!("analysis root does not exist: {}", options.root.display()))?;
    let manifest_path = match options.manifest_path {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => root.join(path),
        None => root.join("Cargo.toml"),
    };
    if !manifest_path.is_file() {
        bail!("Cargo manifest does not exist: {}", manifest_path.display());
    }

    let mut command = MetadataCommand::new();
    command.current_dir(&root).no_deps();
    command.other_options(vec!["--offline".to_owned()]);
    command.manifest_path(&manifest_path);
    let metadata = command
        .exec()
        .context("cargo metadata failed in offline mode")?;
    let workspace_root = metadata
        .workspace_root
        .clone()
        .into_std_path_buf()
        .canonicalize()
        .context("Cargo workspace root could not be canonicalized")?;
    let targets = build_targets(&metadata.packages)?;
    if targets.is_empty() {
        bail!("Cargo workspace has no analyzable Rust targets");
    }

    let mut analyzer = Analyzer {
        workspace_root,
        strict: options.strict,
        targets: targets
            .into_iter()
            .map(|target| (target.root_id.clone(), target))
            .collect(),
        graph: DependencyGraph::default(),
        raw_dependencies: Vec::new(),
        module_paths: BTreeMap::new(),
        module_locations: BTreeMap::new(),
        module_items: BTreeMap::new(),
        module_type_items: BTreeMap::new(),
        module_sources: BTreeMap::new(),
        active_sources: BTreeSet::new(),
    };

    let targets = analyzer.targets.values().cloned().collect::<Vec<_>>();
    for target in targets {
        let module_dir = target
            .source
            .parent()
            .context("Cargo target source has no parent directory")?
            .to_path_buf();
        analyzer.discover_file_module(
            &target,
            Vec::new(),
            &target.source,
            &module_dir,
            None,
            None,
        )?;
    }
    analyzer.resolve_dependencies();

    Ok(analyzer.graph)
}

fn build_targets(packages: &[Package]) -> anyhow::Result<Vec<TargetInfo>> {
    let mut package_targets =
        BTreeMap::<String, Vec<(String, String, PathBuf, bool, Edition)>>::new();
    let mut package_dependencies = BTreeMap::new();

    for package in packages {
        let package_name = package.name.to_string();
        package_dependencies.insert(package_name.clone(), package.dependencies.clone());
        for target in package
            .targets
            .iter()
            .filter(|target| !target.is_custom_build())
        {
            let kind = target_kind(target);
            let root_id = format!("{}::{}({})", package_name, kind, target.name);
            package_targets
                .entry(package_name.clone())
                .or_default()
                .push((
                    target.name.clone(),
                    root_id,
                    target.src_path.clone().into_std_path_buf(),
                    target.is_lib()
                        || target.is_rlib()
                        || target.is_dylib()
                        || target.is_cdylib()
                        || target.is_staticlib()
                        || target.is_proc_macro(),
                    target.edition,
                ));
        }
    }
    for targets in package_targets.values_mut() {
        targets.sort();
    }

    let library_roots = package_targets
        .iter()
        .filter_map(|(package, targets)| {
            targets
                .iter()
                .find(|(_, _, _, is_library, _)| *is_library)
                .map(|(name, root, _, _, _)| (package.clone(), (name.clone(), root.clone())))
        })
        .collect::<BTreeMap<_, _>>();

    let mut result = Vec::new();
    for (package, targets) in &package_targets {
        let dependencies = package_dependencies
            .get(package)
            .context("package dependency metadata is missing")?;
        for (target_name, root_id, source, is_library, edition) in targets {
            let mut workspace_aliases = BTreeMap::new();
            let mut external_aliases = BTreeSet::new();
            for dependency in dependencies {
                if let Some((library_name, root)) = library_roots.get(dependency.name.as_str()) {
                    let alias = dependency
                        .rename
                        .as_deref()
                        .unwrap_or(library_name)
                        .replace('-', "_");
                    workspace_aliases.insert(alias, root.clone());
                } else {
                    let alias = dependency
                        .rename
                        .as_deref()
                        .unwrap_or(dependency.name.as_str())
                        .replace('-', "_");
                    external_aliases.insert(alias);
                }
            }
            if !is_library {
                if let Some((library_name, library_root)) = library_roots.get(package) {
                    workspace_aliases.insert(library_name.replace('-', "_"), library_root.clone());
                }
            }

            result.push(TargetInfo {
                package: package.clone(),
                name: target_name.clone(),
                root_id: root_id.clone(),
                source: source.clone(),
                edition: *edition,
                workspace_aliases,
                external_aliases,
            });
        }
    }
    result.sort_by(|left, right| left.root_id.cmp(&right.root_id));
    Ok(result)
}

fn target_kind(target: &Target) -> &'static str {
    if target.is_lib()
        || target.is_rlib()
        || target.is_dylib()
        || target.is_cdylib()
        || target.is_staticlib()
    {
        "lib"
    } else if target.is_proc_macro() {
        "proc-macro"
    } else if target.is_bin() {
        "bin"
    } else if target.is_example() {
        "example"
    } else if target.is_test() {
        "test"
    } else if target.is_bench() {
        "bench"
    } else {
        "target"
    }
}

impl Analyzer {
    fn discover_file_module(
        &mut self,
        target: &TargetInfo,
        segments: Vec<String>,
        source: &Path,
        module_dir: &Path,
        declaring_source: Option<&Path>,
        declaration_line: Option<usize>,
    ) -> anyhow::Result<()> {
        let module_id = module_id(&target.root_id, &segments);
        let canonical_source = match source.canonicalize() {
            Ok(source) => source,
            Err(error) => {
                self.add_diagnostic(
                    "source-read-failed",
                    format!("could not canonicalize Rust source: {error}"),
                    declaring_source.or(Some(source)),
                    declaration_line,
                );
                return Ok(());
            }
        };
        if !canonical_source.starts_with(&self.workspace_root) {
            self.add_diagnostic(
                "module-outside-workspace",
                format!("module '{module_id}' resolves outside the Cargo workspace"),
                declaring_source.or(Some(source)),
                declaration_line,
            );
            return Ok(());
        }
        if self.active_sources.contains(&canonical_source) {
            self.add_diagnostic(
                "recursive-module-source",
                format!(
                    "module '{module_id}' recursively resolves to active source '{}'",
                    self.normalize_path(&canonical_source)
                ),
                declaring_source.or(Some(source)),
                declaration_line,
            );
            return Ok(());
        }
        if let Some(existing) = self.module_sources.get(&module_id) {
            if existing == &canonical_source {
                return Ok(());
            }
            self.add_diagnostic(
                "cfg-ambiguous-module",
                format!(
                    "module '{module_id}' resolves to both '{}' and '{}' across syntactic branches",
                    self.normalize_path(existing),
                    self.normalize_path(&canonical_source)
                ),
                declaring_source.or(Some(source)),
                declaration_line,
            );
            return Ok(());
        }
        self.module_sources
            .insert(module_id.clone(), canonical_source.clone());
        self.active_sources.insert(canonical_source.clone());

        let contents = match fs::read_to_string(&canonical_source) {
            Ok(contents) => contents,
            Err(error) => {
                self.add_diagnostic(
                    "source-read-failed",
                    format!("could not read Rust source: {error}"),
                    Some(&canonical_source),
                    None,
                );
                self.active_sources.remove(&canonical_source);
                return Ok(());
            }
        };
        let file = match syn::parse_file(&contents) {
            Ok(file) => file,
            Err(error) => {
                self.add_diagnostic(
                    "parse-failed",
                    error.to_string(),
                    Some(&canonical_source),
                    Some(error.span().start().line),
                );
                self.active_sources.remove(&canonical_source);
                return Ok(());
            }
        };

        self.register_module(target, &segments, &canonical_source);
        let result = self.inspect_items(
            target,
            &segments,
            &canonical_source,
            module_dir,
            canonical_source.parent().unwrap_or(module_dir),
            &file.items,
        );
        self.active_sources.remove(&canonical_source);
        result
    }

    fn inspect_items(
        &mut self,
        target: &TargetInfo,
        segments: &[String],
        source: &Path,
        module_dir: &Path,
        path_attribute_base: &Path,
        items: &[Item],
    ) -> anyhow::Result<()> {
        self.register_items(target, segments, items);
        let current_module_id = module_id(&target.root_id, segments);
        let source_normalized = self.normalize_path(source);
        let mut visitor = DependencyVisitor::new(
            &current_module_id,
            &target.root_id,
            segments,
            &source_normalized,
            self.strict,
        );
        for item in items {
            visitor.visit_item(item);
        }
        self.raw_dependencies.extend(visitor.dependencies);
        self.graph.diagnostics.extend(visitor.diagnostics);

        for item in items {
            let Item::Mod(item_mod) = item else {
                continue;
            };
            let child_name = item_mod.ident.to_string();
            let mut child_segments = segments.to_vec();
            child_segments.push(child_name.clone());
            if let Some((_, inline_items)) = &item_mod.content {
                let child_id = module_id(&target.root_id, &child_segments);
                let canonical_source = source
                    .canonicalize()
                    .expect("discovered source was already canonicalized");
                if let Some(existing) = self.module_sources.get(&child_id) {
                    if existing != &canonical_source {
                        self.add_diagnostic(
                            "cfg-ambiguous-module",
                            format!("inline module '{child_id}' has ambiguous sources"),
                            Some(source),
                            Some(item_mod.span().start().line),
                        );
                        continue;
                    }
                } else {
                    self.module_sources
                        .insert(child_id.clone(), canonical_source);
                    self.register_module(target, &child_segments, source);
                }
                let child_dir = match module_path_attribute(&item_mod.attrs) {
                    Ok(Some(custom_path)) => module_dir.join(custom_path),
                    Ok(None) => module_dir.join(&child_name),
                    Err(message) => {
                        self.add_diagnostic(
                            "unresolved-module",
                            message,
                            Some(source),
                            Some(item_mod.span().start().line),
                        );
                        continue;
                    }
                };
                self.inspect_items(
                    target,
                    &child_segments,
                    source,
                    &child_dir,
                    &child_dir,
                    inline_items,
                )?;
                continue;
            }

            let line = item_mod.span().start().line;
            match resolve_module_file(
                path_attribute_base,
                module_dir,
                &child_name,
                &item_mod.attrs,
            ) {
                Ok((child_source, child_dir)) => {
                    self.discover_file_module(
                        target,
                        child_segments,
                        &child_source,
                        &child_dir,
                        Some(source),
                        Some(line),
                    )?;
                }
                Err(message) => {
                    self.add_diagnostic("unresolved-module", message, Some(source), Some(line))
                }
            }
        }

        Ok(())
    }

    fn register_module(&mut self, target: &TargetInfo, segments: &[String], source: &Path) {
        let id = module_id(&target.root_id, segments);
        let relative = segments.join("::");
        self.module_paths
            .insert((target.root_id.clone(), relative), id.clone());
        self.module_locations
            .insert(id.clone(), (target.root_id.clone(), segments.to_vec()));
        self.graph.modules.insert(
            id.clone(),
            Module {
                id: id.clone(),
                package: target.package.clone(),
                target: target.root_id.clone(),
                module: id,
                source: self.normalize_path(source),
            },
        );
    }

    fn register_items(&mut self, target: &TargetInfo, segments: &[String], items: &[Item]) {
        let location = (target.root_id.clone(), segments.join("::"));
        let names = items.iter().filter_map(item_name).collect::<BTreeSet<_>>();
        self.module_items
            .entry(location.clone())
            .or_default()
            .extend(names);
        let type_names = items
            .iter()
            .filter_map(type_item_name)
            .collect::<BTreeSet<_>>();
        self.module_type_items
            .entry(location)
            .or_default()
            .extend(type_names);
    }

    fn resolve_dependencies(&mut self) {
        self.raw_dependencies.sort_by(|left, right| {
            (
                &left.source,
                &left.evidence.path,
                left.evidence.line,
                &left.evidence.expression,
            )
                .cmp(&(
                    &right.source,
                    &right.evidence.path,
                    right.evidence.line,
                    &right.evidence.expression,
                ))
        });
        let mut dependencies = BTreeMap::<(String, String), Dependency>::new();
        let mut imports = BTreeMap::<(String, String), ImportBinding>::new();
        let raw_dependencies = std::mem::take(&mut self.raw_dependencies);
        let mut pending_imports = raw_dependencies
            .iter()
            .filter(|raw| raw.imported_name.is_some())
            .collect::<Vec<_>>();
        while !pending_imports.is_empty() {
            let mut unresolved = Vec::new();
            let mut progress = false;
            for raw in pending_imports {
                let resolution = self.resolve_dependency(raw, &imports);
                let Resolution::Unresolved(_) = resolution else {
                    let imported_name = raw
                        .imported_name
                        .as_ref()
                        .expect("import dependencies have an exposed name")
                        .clone();
                    let mut binding = ImportBinding::from_resolution(resolution);
                    binding.global |= raw.global_import;
                    for target in &binding.targets {
                        for (via, exported_name) in &binding.opaque_routes {
                            self.graph.opaque_reexports.insert(OpaqueReexport {
                                source: raw.source.clone(),
                                target: target.module.clone(),
                                via: via.clone(),
                                exported_name: exported_name.clone(),
                                evidence: raw.evidence.clone(),
                            });
                        }
                    }
                    if raw.public_import {
                        binding
                            .opaque_routes
                            .insert((raw.source.clone(), imported_name.clone()));
                    }
                    for target in &binding.targets {
                        if target.module != raw.source {
                            dependencies
                                .entry((raw.source.clone(), target.module.clone()))
                                .or_insert(Dependency {
                                    source: raw.source.clone(),
                                    target: target.module.clone(),
                                    evidence: raw.evidence.clone(),
                                });
                        }
                    }
                    imports
                        .entry((raw.source.clone(), imported_name))
                        .or_default()
                        .merge(binding);
                    progress = true;
                    continue;
                };
                unresolved.push(raw);
            }
            if !progress {
                for raw in unresolved {
                    let Resolution::Unresolved(message) = self.resolve_dependency(raw, &imports)
                    else {
                        unreachable!("an import cannot become resolvable without progress")
                    };
                    self.record_unresolved(raw, message);
                }
                break;
            }
            pending_imports = unresolved;
        }

        for raw in raw_dependencies
            .iter()
            .filter(|raw| raw.imported_name.is_none())
        {
            match self.resolve_dependency(raw, &imports) {
                Resolution::Module { target, .. } if target != raw.source => {
                    dependencies
                        .entry((raw.source.clone(), target.clone()))
                        .or_insert(Dependency {
                            source: raw.source.clone(),
                            target,
                            evidence: raw.evidence.clone(),
                        });
                }
                Resolution::Imported(binding) => {
                    for target in &binding.targets {
                        if target.module != raw.source {
                            dependencies
                                .entry((raw.source.clone(), target.module.clone()))
                                .or_insert(Dependency {
                                    source: raw.source.clone(),
                                    target: target.module.clone(),
                                    evidence: raw.evidence.clone(),
                                });
                        }
                        for (via, exported_name) in &binding.opaque_routes {
                            self.graph.opaque_reexports.insert(OpaqueReexport {
                                source: raw.source.clone(),
                                target: target.module.clone(),
                                via: via.clone(),
                                exported_name: exported_name.clone(),
                                evidence: raw.evidence.clone(),
                            });
                        }
                    }
                }
                Resolution::Module { .. } | Resolution::External | Resolution::LocalItem => {}
                Resolution::Unresolved(message) => self.record_unresolved(raw, message),
            }
        }
        self.graph.dependencies = dependencies.into_values().collect();
    }

    fn record_unresolved(&mut self, raw: &RawDependency, message: String) {
        self.graph.diagnostics.insert(AnalysisDiagnostic {
            code: "unresolved-import".to_owned(),
            message,
            path: Some(raw.evidence.path.clone()),
            line: Some(raw.evidence.line),
        });
    }

    fn resolve_dependency(
        &self,
        raw: &RawDependency,
        imports: &BTreeMap<(String, String), ImportBinding>,
    ) -> Resolution {
        if raw.segments.is_empty() {
            return Resolution::LocalItem;
        }
        let target = self
            .targets
            .get(&raw.target_root)
            .expect("raw dependency refers to a known target");
        let mut segments = raw.segments.as_slice();
        let mut target_root = raw.target_root.as_str();
        let mut candidate = Vec::new();

        let first = segments[0].as_str();
        if raw.leading_colon {
            if target.edition == Edition::E2015 {
                let crate_root_resolution = self.resolve_candidate(
                    &raw.target_root,
                    segments,
                    imports,
                    &raw.evidence.expression,
                    &target.name,
                );
                if !matches!(&crate_root_resolution, Resolution::Unresolved(_)) {
                    return crate_root_resolution;
                }
                if let Some(workspace_root) = target.workspace_aliases.get(first) {
                    return self.resolve_candidate(
                        workspace_root,
                        &segments[1..],
                        imports,
                        &raw.evidence.expression,
                        &target.name,
                    );
                }
                if target.external_aliases.contains(first) || STANDARD_CRATES.contains(&first) {
                    return Resolution::External;
                }
                return crate_root_resolution;
            }
            if let Some(workspace_root) = target.workspace_aliases.get(first) {
                return self.resolve_candidate(
                    workspace_root,
                    &segments[1..],
                    imports,
                    &raw.evidence.expression,
                    &target.name,
                );
            }
            if target.external_aliases.contains(first) || STANDARD_CRATES.contains(&first) {
                return Resolution::External;
            }
            return Resolution::Unresolved(format!(
                "could not resolve absolute import root '{}' in target '{}'",
                first, target.name
            ));
        }
        if !matches!(first, "crate" | "self" | "super") {
            let crate_alias_takes_precedence = target.workspace_aliases.contains_key(first)
                || target.external_aliases.contains(first)
                || STANDARD_CRATES.contains(&first);
            if let Some(binding) = imports.get(&(raw.source.clone(), first.to_owned())) {
                return self.resolve_import_tail(
                    binding,
                    &segments[1..],
                    imports,
                    &raw.evidence.expression,
                );
            }
            let mut local_candidate = raw.current_segments.clone();
            local_candidate.extend(segments.iter().cloned());
            let mut local_module = raw.current_segments.clone();
            local_module.push(first.to_owned());
            let local_module_exists = self
                .module_paths
                .contains_key(&(raw.target_root.clone(), local_module.join("::")));
            let local_type_item_exists = self
                .module_type_items
                .get(&(raw.target_root.clone(), raw.current_segments.join("::")))
                .is_some_and(|items| items.contains(first));
            if !crate_alias_takes_precedence || local_module_exists || local_type_item_exists {
                let local_resolution = self.resolve_candidate(
                    &raw.target_root,
                    &local_candidate,
                    imports,
                    &raw.evidence.expression,
                    &target.name,
                );
                if !matches!(local_resolution, Resolution::Unresolved(_)) {
                    return local_resolution;
                }
            }
            if !crate_alias_takes_precedence
                && self
                    .module_items
                    .get(&(raw.target_root.clone(), raw.current_segments.join("::")))
                    .is_some_and(|items| items.contains(first))
            {
                return Resolution::LocalItem;
            }
            let root_module = module_id(&raw.target_root, &[]);
            if let Some(binding) = imports
                .get(&(root_module, first.to_owned()))
                .filter(|binding| binding.global)
            {
                return self.resolve_import_tail(
                    binding,
                    &segments[1..],
                    imports,
                    &raw.evidence.expression,
                );
            }
        }

        match first {
            "crate" => segments = &segments[1..],
            "self" => {
                candidate = raw.current_segments.clone();
                segments = &segments[1..];
            }
            "super" => {
                candidate = raw.current_segments.clone();
                while segments.first().is_some_and(|segment| segment == "super") {
                    if candidate.pop().is_none() {
                        return Resolution::Unresolved(format!(
                            "'{}' traverses beyond the crate root",
                            raw.evidence.expression
                        ));
                    }
                    segments = &segments[1..];
                }
            }
            first if target.workspace_aliases.contains_key(first) => {
                target_root = target
                    .workspace_aliases
                    .get(first)
                    .expect("workspace alias was checked");
                segments = &segments[1..];
            }
            first if target.external_aliases.contains(first) => return Resolution::External,
            first if STANDARD_CRATES.contains(&first) => return Resolution::External,
            first => {
                if let Some(resolution) =
                    self.resolve_top_level_module(raw, segments, imports, &target.name)
                {
                    return resolution;
                }
                if STANDARD_PRELUDE_NAMES.contains(&first) {
                    return Resolution::External;
                }
                if self.is_crate_root_item(raw, first) {
                    return Resolution::LocalItem;
                }
                return if raw.origin == DependencyOrigin::Path {
                    Resolution::LocalItem
                } else {
                    Resolution::Unresolved(format!(
                        "could not resolve import root '{}' in target '{}'",
                        first, target.name
                    ))
                };
            }
        }
        candidate.extend(segments.iter().cloned());

        let resolution = self.resolve_candidate(
            target_root,
            &candidate,
            imports,
            &raw.evidence.expression,
            &target.name,
        );
        if matches!(resolution, Resolution::Unresolved(_))
            && raw.origin == DependencyOrigin::Path
            && !matches!(raw.segments[0].as_str(), "crate" | "self" | "super")
        {
            return Resolution::LocalItem;
        }
        resolution
    }

    fn resolve_top_level_module(
        &self,
        raw: &RawDependency,
        segments: &[String],
        imports: &BTreeMap<(String, String), ImportBinding>,
        target_name: &str,
    ) -> Option<Resolution> {
        self.module_paths
            .contains_key(&(raw.target_root.clone(), segments[0].clone()))
            .then(|| {
                self.resolve_candidate(
                    &raw.target_root,
                    segments,
                    imports,
                    &raw.evidence.expression,
                    target_name,
                )
            })
    }

    fn is_crate_root_item(&self, raw: &RawDependency, name: &str) -> bool {
        self.module_items
            .get(&(raw.target_root.clone(), String::new()))
            .is_some_and(|items| items.contains(name))
    }

    fn resolve_candidate(
        &self,
        target_root: &str,
        candidate: &[String],
        imports: &BTreeMap<(String, String), ImportBinding>,
        expression: &str,
        target_name: &str,
    ) -> Resolution {
        for length in (0..=candidate.len()).rev() {
            let relative = candidate[..length].join("::");
            if let Some(module) = self
                .module_paths
                .get(&(target_root.to_owned(), relative.clone()))
            {
                let remaining = &candidate[length..];
                if remaining.is_empty() {
                    return Resolution::Module {
                        target: module.clone(),
                        exact_module: true,
                    };
                }
                let exposed_name = &remaining[0];
                if let Some(binding) = imports.get(&(module.clone(), exposed_name.clone())) {
                    return self.resolve_import_tail(binding, &remaining[1..], imports, expression);
                }
                if let Some(binding) = imports.get(&(module.clone(), "*".to_owned())) {
                    return self.resolve_import_tail(binding, remaining, imports, expression);
                }
                if self
                    .module_items
                    .get(&(target_root.to_owned(), relative))
                    .is_some_and(|items| items.contains(exposed_name))
                {
                    return Resolution::Module {
                        target: module.clone(),
                        exact_module: false,
                    };
                }
                return Resolution::Unresolved(format!(
                    "could not resolve '{}' after module '{}'",
                    remaining.join("::"),
                    module
                ));
            }
        }

        Resolution::Unresolved(format!(
            "could not resolve '{}' in target '{}'",
            expression, target_name
        ))
    }

    fn resolve_import_tail(
        &self,
        binding: &ImportBinding,
        tail: &[String],
        imports: &BTreeMap<(String, String), ImportBinding>,
        expression: &str,
    ) -> Resolution {
        if tail.is_empty() {
            return Resolution::Imported(binding.clone());
        }
        let mut resolved = ImportBinding {
            external: binding.external,
            local: binding.local,
            global: binding.global,
            opaque_routes: binding.opaque_routes.clone(),
            ..ImportBinding::default()
        };
        for target in &binding.targets {
            if !target.exact_module {
                resolved.targets.insert(target.clone());
                continue;
            }
            let Some((target_root, segments)) = self.module_locations.get(&target.module) else {
                continue;
            };
            let mut candidate = segments.clone();
            candidate.extend(tail.iter().cloned());
            let target_name = self
                .targets
                .get(target_root)
                .map_or(target_root.as_str(), |target| target.name.as_str());
            match self.resolve_candidate(target_root, &candidate, imports, expression, target_name)
            {
                Resolution::Module {
                    target,
                    exact_module,
                } => {
                    resolved.targets.insert(ImportTarget {
                        module: target,
                        exact_module,
                    });
                }
                Resolution::Imported(nested) => resolved.merge(nested),
                Resolution::External => resolved.external = true,
                Resolution::LocalItem => resolved.local = true,
                Resolution::Unresolved(_) => {}
            }
        }
        if resolved.targets.is_empty() && !resolved.external && !resolved.local {
            Resolution::Unresolved(format!(
                "could not resolve '{expression}' through import alias"
            ))
        } else {
            Resolution::Imported(resolved)
        }
    }

    fn add_diagnostic(
        &mut self,
        code: &str,
        message: String,
        path: Option<&Path>,
        line: Option<usize>,
    ) {
        self.graph.diagnostics.insert(AnalysisDiagnostic {
            code: code.to_owned(),
            message,
            path: path.map(|path| self.normalize_path(path)),
            line,
        });
    }

    fn normalize_path(&self, path: &Path) -> String {
        let relative = path
            .strip_prefix(&self.workspace_root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| relative_path(&self.workspace_root, path));
        normalize_path(&relative)
    }
}

enum Resolution {
    Module { target: String, exact_module: bool },
    Imported(ImportBinding),
    External,
    LocalItem,
    Unresolved(String),
}

impl ImportBinding {
    fn from_resolution(resolution: Resolution) -> Self {
        match resolution {
            Resolution::Module {
                target,
                exact_module,
            } => Self {
                targets: BTreeSet::from([ImportTarget {
                    module: target,
                    exact_module,
                }]),
                ..Self::default()
            },
            Resolution::Imported(binding) => binding,
            Resolution::External => Self {
                external: true,
                ..Self::default()
            },
            Resolution::LocalItem => Self {
                local: true,
                ..Self::default()
            },
            Resolution::Unresolved(_) => {
                unreachable!("unresolved imports cannot become bindings")
            }
        }
    }
}

struct DependencyVisitor<'a> {
    source: &'a str,
    target_root: &'a str,
    current_segments: &'a [String],
    source_path: &'a str,
    strict: bool,
    lexical_type_scopes: Vec<BTreeSet<String>>,
    dependencies: Vec<RawDependency>,
    diagnostics: BTreeSet<AnalysisDiagnostic>,
}

impl<'a> DependencyVisitor<'a> {
    fn new(
        source: &'a str,
        target_root: &'a str,
        current_segments: &'a [String],
        source_path: &'a str,
        strict: bool,
    ) -> Self {
        Self {
            source,
            target_root,
            current_segments,
            source_path,
            strict,
            lexical_type_scopes: Vec::new(),
            dependencies: Vec::new(),
            diagnostics: BTreeSet::new(),
        }
    }

    fn push_generic_scope(&mut self, generics: &syn::Generics) {
        self.lexical_type_scopes.push(
            generics
                .params
                .iter()
                .filter_map(|parameter| match parameter {
                    syn::GenericParam::Type(parameter) => Some(parameter.ident.to_string()),
                    syn::GenericParam::Lifetime(_) | syn::GenericParam::Const(_) => None,
                })
                .collect(),
        );
    }

    fn pop_type_scope(&mut self) {
        self.lexical_type_scopes
            .pop()
            .expect("type scope pushes and pops must remain balanced");
    }

    fn has_lexical_type_root(&self, path: &SynPath) -> bool {
        path.leading_colon.is_none()
            && path.segments.first().is_some_and(|segment| {
                self.lexical_type_scopes
                    .iter()
                    .rev()
                    .any(|scope| scope.contains(&segment.ident.to_string()))
            })
    }

    fn push_dependency(
        &mut self,
        path: DependencyPath,
        origin: DependencyOrigin,
        imported_name: Option<String>,
        public_import: bool,
        global_import: bool,
        span: Span,
    ) {
        let DependencyPath {
            segments,
            leading_colon,
        } = path;
        if segments.is_empty() {
            return;
        }
        self.dependencies.push(RawDependency {
            source: self.source.to_owned(),
            target_root: self.target_root.to_owned(),
            current_segments: self.current_segments.to_vec(),
            evidence: Evidence {
                path: self.source_path.to_owned(),
                line: span.start().line,
                expression: format!(
                    "{}{}",
                    if leading_colon { "::" } else { "" },
                    segments.join("::")
                ),
            },
            segments,
            leading_colon,
            origin,
            imported_name,
            public_import,
            global_import,
        });
    }

    fn record_unsupported_macro(&mut self, node: &Macro, code: &str) {
        self.diagnostics.insert(AnalysisDiagnostic {
            code: code.to_owned(),
            message: format!(
                "macro '{}' is not expanded during dependency analysis",
                path_segments(&node.path).join("::")
            ),
            path: Some(self.source_path.to_owned()),
            line: Some(node.span().start().line),
        });
    }
}

impl<'ast> Visit<'ast> for DependencyVisitor<'_> {
    fn visit_block(&mut self, node: &'ast syn::Block) {
        self.lexical_type_scopes.push(
            node.stmts
                .iter()
                .filter_map(|statement| match statement {
                    syn::Stmt::Item(item) => type_item_name(item),
                    syn::Stmt::Local(_) | syn::Stmt::Expr(_, _) | syn::Stmt::Macro(_) => None,
                })
                .collect(),
        );
        visit::visit_block(self, node);
        self.pop_type_scope();
    }

    fn visit_foreign_item_fn(&mut self, node: &'ast syn::ForeignItemFn) {
        self.push_generic_scope(&node.sig.generics);
        visit::visit_foreign_item_fn(self, node);
        self.pop_type_scope();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.push_generic_scope(&node.sig.generics);
        visit::visit_impl_item_fn(self, node);
        self.pop_type_scope();
    }

    fn visit_impl_item_type(&mut self, node: &'ast syn::ImplItemType) {
        self.push_generic_scope(&node.generics);
        visit::visit_impl_item_type(self, node);
        self.pop_type_scope();
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.push_generic_scope(&node.generics);
        visit::visit_item_enum(self, node);
        self.pop_type_scope();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.push_generic_scope(&node.sig.generics);
        visit::visit_item_fn(self, node);
        self.pop_type_scope();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.push_generic_scope(&node.generics);
        visit::visit_item_impl(self, node);
        self.pop_type_scope();
    }

    fn visit_item_mod(&mut self, _node: &'ast syn::ItemMod) {}

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.push_generic_scope(&node.generics);
        visit::visit_item_struct(self, node);
        self.pop_type_scope();
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.push_generic_scope(&node.generics);
        visit::visit_item_trait(self, node);
        self.pop_type_scope();
    }

    fn visit_item_trait_alias(&mut self, node: &'ast syn::ItemTraitAlias) {
        self.push_generic_scope(&node.generics);
        visit::visit_item_trait_alias(self, node);
        self.pop_type_scope();
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        self.push_generic_scope(&node.generics);
        visit::visit_item_type(self, node);
        self.pop_type_scope();
    }

    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        self.push_generic_scope(&node.generics);
        visit::visit_item_union(self, node);
        self.pop_type_scope();
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.push_generic_scope(&node.sig.generics);
        visit::visit_trait_item_fn(self, node);
        self.pop_type_scope();
    }

    fn visit_trait_item_type(&mut self, node: &'ast syn::TraitItemType) {
        self.push_generic_scope(&node.generics);
        visit::visit_trait_item_type(self, node);
        self.pop_type_scope();
    }

    fn visit_item_use(&mut self, node: &'ast ItemUse) {
        let is_public = !matches!(node.vis, Visibility::Inherited);
        match flatten_item_use(node) {
            Ok(imports) => {
                for import in imports {
                    self.push_dependency(
                        DependencyPath {
                            segments: import.segments,
                            leading_colon: import.leading_colon,
                        },
                        DependencyOrigin::Use,
                        Some(import.exposed_name),
                        is_public,
                        false,
                        node.span(),
                    );
                }
            }
            Err(message) => {
                self.diagnostics.insert(AnalysisDiagnostic {
                    code: "unresolved-import".to_owned(),
                    message,
                    path: Some(self.source_path.to_owned()),
                    line: Some(node.span().start().line),
                });
            }
        }
    }

    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        let exposed_name = node
            .rename
            .as_ref()
            .map_or_else(|| node.ident.to_string(), |(_, rename)| rename.to_string());
        let segments = if node.ident == "self" {
            vec!["crate".to_owned()]
        } else {
            vec![node.ident.to_string()]
        };
        self.push_dependency(
            DependencyPath {
                segments,
                leading_colon: false,
            },
            DependencyOrigin::Use,
            Some(exposed_name),
            !matches!(node.vis, Visibility::Inherited),
            self.current_segments.is_empty(),
            node.span(),
        );
    }

    fn visit_path(&mut self, node: &'ast SynPath) {
        let segments = path_segments(node);
        if !self.has_lexical_type_root(node)
            && (segments.len() > 1
                || segments
                    .first()
                    .is_some_and(|segment| matches!(segment.as_str(), "crate" | "self" | "super")))
        {
            self.push_dependency(
                DependencyPath {
                    segments,
                    leading_colon: node.leading_colon.is_some(),
                },
                DependencyOrigin::Path,
                None,
                false,
                false,
                node.span(),
            );
        }
        visit::visit_path(self, node);
    }

    fn visit_item_macro(&mut self, node: &'ast ItemMacro) {
        if node.ident.is_some() {
            return;
        }
        let segments = path_segments(&node.mac.path);
        if segments.last().is_some_and(|name| name == "include") {
            self.record_unsupported_macro(&node.mac, "unsupported-include");
        } else if self.strict {
            self.record_unsupported_macro(&node.mac, "unsupported-item-macro");
        }
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        if path_segments(&node.path)
            .last()
            .is_some_and(|name| name == "include")
        {
            self.record_unsupported_macro(node, "unsupported-include");
        }
        visit::visit_macro(self, node);
    }
}

fn flatten_item_use(item_use: &ItemUse) -> Result<Vec<UseImport>, String> {
    let mut imports = Vec::new();
    flatten_use_tree(&item_use.tree, Vec::new(), &mut imports)?;
    for import in &mut imports {
        import.leading_colon = item_use.leading_colon.is_some();
    }
    Ok(imports)
}

fn flatten_use_tree(
    tree: &UseTree,
    prefix: Vec<String>,
    imports: &mut Vec<UseImport>,
) -> Result<(), String> {
    match tree {
        UseTree::Path(path) => {
            let mut next = prefix;
            next.push(path.ident.to_string());
            flatten_use_tree(&path.tree, next, imports)?;
        }
        UseTree::Name(name) => {
            let mut path = prefix;
            if name.ident != "self" {
                path.push(name.ident.to_string());
            }
            let Some(exposed_name) = path.last().cloned() else {
                return Err("invalid use tree has `self` without a path prefix".to_owned());
            };
            imports.push(UseImport {
                segments: path,
                exposed_name,
                leading_colon: false,
            });
        }
        UseTree::Rename(rename) => {
            let mut path = prefix;
            path.push(rename.ident.to_string());
            imports.push(UseImport {
                segments: path,
                exposed_name: rename.rename.to_string(),
                leading_colon: false,
            });
        }
        UseTree::Glob(_) => imports.push(UseImport {
            segments: prefix,
            exposed_name: "*".to_owned(),
            leading_colon: false,
        }),
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use_tree(item, prefix.clone(), imports)?;
            }
        }
    }
    Ok(())
}

fn path_segments(path: &SynPath) -> Vec<String> {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect()
}

fn item_name(item: &Item) -> Option<String> {
    match item {
        Item::Const(item) => Some(item.ident.to_string()),
        Item::Enum(item) => Some(item.ident.to_string()),
        Item::Fn(item) => Some(item.sig.ident.to_string()),
        Item::Macro(item) => item.ident.as_ref().map(ToString::to_string),
        Item::Mod(item) => Some(item.ident.to_string()),
        Item::Static(item) => Some(item.ident.to_string()),
        Item::Struct(item) => Some(item.ident.to_string()),
        Item::Trait(item) => Some(item.ident.to_string()),
        Item::TraitAlias(item) => Some(item.ident.to_string()),
        Item::Type(item) => Some(item.ident.to_string()),
        Item::Union(item) => Some(item.ident.to_string()),
        _ => None,
    }
}

fn type_item_name(item: &Item) -> Option<String> {
    match item {
        Item::Enum(item) if !has_conditional_compilation(&item.attrs) => {
            Some(item.ident.to_string())
        }
        Item::Struct(item) if !has_conditional_compilation(&item.attrs) => {
            Some(item.ident.to_string())
        }
        Item::Trait(item) if !has_conditional_compilation(&item.attrs) => {
            Some(item.ident.to_string())
        }
        Item::TraitAlias(item) if !has_conditional_compilation(&item.attrs) => {
            Some(item.ident.to_string())
        }
        Item::Type(item) if !has_conditional_compilation(&item.attrs) => {
            Some(item.ident.to_string())
        }
        Item::Union(item) if !has_conditional_compilation(&item.attrs) => {
            Some(item.ident.to_string())
        }
        _ => None,
    }
}

fn has_conditional_compilation(attributes: &[Attribute]) -> bool {
    attributes
        .iter()
        .any(|attribute| attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr"))
}

fn resolve_module_file(
    path_attribute_base: &Path,
    module_dir: &Path,
    name: &str,
    attributes: &[Attribute],
) -> Result<(PathBuf, PathBuf), String> {
    if let Some(custom_path) = module_path_attribute(attributes)? {
        let source = path_attribute_base.join(&custom_path);
        if !source.is_file() {
            return Err(format!(
                "module '{name}' points to missing #[path] file '{}'",
                custom_path.display()
            ));
        }
        let child_dir = source.parent().unwrap_or(module_dir).to_path_buf();
        return Ok((source, child_dir));
    }

    let flat = module_dir.join(format!("{name}.rs"));
    let nested = module_dir.join(name).join("mod.rs");
    match (flat.is_file(), nested.is_file()) {
        (true, false) => Ok((flat, module_dir.join(name))),
        (false, true) => Ok((nested, module_dir.join(name))),
        (true, true) => Err(format!(
            "module '{name}' is ambiguous: both '{name}.rs' and '{name}/mod.rs' exist"
        )),
        (false, false) => Err(format!(
            "module '{name}' has no source file ('{name}.rs' or '{name}/mod.rs')"
        )),
    }
}

fn module_path_attribute(attributes: &[Attribute]) -> Result<Option<PathBuf>, String> {
    for attribute in attributes {
        if attribute.path().is_ident("cfg_attr") {
            let nested = Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(
                    attribute
                        .meta
                        .require_list()
                        .map_err(|error| error.to_string())?
                        .tokens
                        .clone(),
                )
                .map_err(|error| format!("invalid #[cfg_attr] attribute: {error}"))?;
            if nested.iter().skip(1).any(meta_can_set_module_path) {
                return Err(
                    "conditional #[cfg_attr(..., path = ...)] module paths are unsupported"
                        .to_owned(),
                );
            }
        }
        if !attribute.path().is_ident("path") {
            continue;
        }
        let syn::Meta::NameValue(name_value) = &attribute.meta else {
            return Err("invalid #[path] attribute: expected #[path = \"file.rs\"]".to_owned());
        };
        let syn::Expr::Lit(expression) = &name_value.value else {
            return Err("invalid #[path] attribute: path must be a string literal".to_owned());
        };
        let syn::Lit::Str(value) = &expression.lit else {
            return Err("invalid #[path] attribute: path must be a string literal".to_owned());
        };
        return Ok(Some(PathBuf::from(value.value())));
    }
    Ok(None)
}

fn meta_can_set_module_path(meta: &Meta) -> bool {
    if meta.path().is_ident("path") {
        return true;
    }
    if !meta.path().is_ident("cfg_attr") {
        return false;
    }
    let Meta::List(list) = meta else {
        return false;
    };
    Punctuated::<Meta, Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
        .is_ok_and(|nested| nested.iter().skip(1).any(meta_can_set_module_path))
}

fn module_id(root: &str, segments: &[String]) -> String {
    if segments.is_empty() {
        root.to_owned()
    } else {
        format!("{root}::{}", segments.join("::"))
    }
}

fn normalize_path(path: &Path) -> String {
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.last().is_some_and(|part| part != "..") {
                    normalized.pop();
                } else {
                    normalized.push("..".to_owned());
                }
            }
            Component::Normal(value) => normalized.push(value.to_string_lossy().into_owned()),
            Component::RootDir => normalized.clear(),
            Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
        }
    }
    normalized.join("/")
}

fn relative_path(base: &Path, target: &Path) -> PathBuf {
    let base_components = base.components().collect::<Vec<_>>();
    let target_components = target.components().collect::<Vec<_>>();
    let common = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();

    let mut relative = PathBuf::new();
    for component in &base_components[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &target_components[common..] {
        relative.push(component.as_os_str());
    }
    relative
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_grouped_use_trees() {
        let item: ItemUse = syn::parse_str("use crate::a::{self, B, c::*};").unwrap();
        let imports = flatten_item_use(&item).unwrap();
        assert_eq!(
            imports,
            vec![
                UseImport {
                    segments: vec!["crate".to_owned(), "a".to_owned()],
                    exposed_name: "a".to_owned(),
                    leading_colon: false,
                },
                UseImport {
                    segments: vec!["crate".to_owned(), "a".to_owned(), "B".to_owned()],
                    exposed_name: "B".to_owned(),
                    leading_colon: false,
                },
                UseImport {
                    segments: vec!["crate".to_owned(), "a".to_owned(), "c".to_owned()],
                    exposed_name: "*".to_owned(),
                    leading_colon: false,
                },
            ]
        );
    }

    #[test]
    fn preserves_leading_colon_on_use_imports() {
        let item: ItemUse = syn::parse_str("use ::crate_name::Thing;").unwrap();
        assert_eq!(
            flatten_item_use(&item).unwrap(),
            vec![UseImport {
                segments: vec!["crate_name".to_owned(), "Thing".to_owned()],
                exposed_name: "Thing".to_owned(),
                leading_colon: true,
            }]
        );
    }

    #[test]
    fn normalizes_relative_paths() {
        assert_eq!(
            normalize_path(Path::new("src/./core/../lib.rs")),
            "src/lib.rs"
        );
        assert_eq!(
            normalize_path(Path::new("../shared/lib.rs")),
            "../shared/lib.rs"
        );
    }
}
