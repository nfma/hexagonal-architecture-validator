use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use cargo_metadata::{MetadataCommand, Package, Target};
use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Item, ItemExternCrate, ItemMacro, ItemUse, Macro, Path as SynPath, UseTree,
    Visibility,
};

use crate::model::{
    AnalysisDiagnostic, Dependency, DependencyGraph, Evidence, Module, OpaqueReexport,
};

const STANDARD_CRATES: [&str; 5] = ["alloc", "core", "proc_macro", "std", "test"];

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
    origin: DependencyOrigin,
    reexported_name: Option<String>,
    evidence: Evidence,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct UseImport {
    segments: Vec<String>,
    exposed_name: String,
}

struct Analyzer {
    workspace_root: PathBuf,
    strict: bool,
    targets: BTreeMap<String, TargetInfo>,
    graph: DependencyGraph,
    raw_dependencies: Vec<RawDependency>,
    module_paths: BTreeMap<(String, String), String>,
    module_items: BTreeMap<(String, String), BTreeSet<String>>,
    module_sources: BTreeMap<String, PathBuf>,
    active_sources: BTreeSet<PathBuf>,
}

pub fn analyze(options: AnalysisOptions<'_>) -> anyhow::Result<DependencyGraph> {
    let root = options
        .root
        .canonicalize()
        .with_context(|| format!("analysis root does not exist: {}", options.root.display()))?;
    let manifest_path = options.manifest_path.map(|path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        }
    });

    let mut command = MetadataCommand::new();
    command.current_dir(&root).no_deps();
    command.other_options(vec!["--offline".to_owned()]);
    if let Some(path) = &manifest_path {
        command.manifest_path(path);
    }
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
        module_items: BTreeMap::new(),
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
    let mut package_targets = BTreeMap::<String, Vec<(String, String, PathBuf, bool)>>::new();
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
                .find(|(_, _, _, is_library)| *is_library)
                .map(|(name, root, _, _)| (package.clone(), (name.clone(), root.clone())))
        })
        .collect::<BTreeMap<_, _>>();

    let mut result = Vec::new();
    for (package, targets) in &package_targets {
        let dependencies = package_dependencies
            .get(package)
            .context("package dependency metadata is missing")?;
        for (target_name, root_id, source, is_library) in targets {
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
            &module_id,
            &segments,
            &canonical_source,
            module_dir,
            &file.items,
        );
        self.active_sources.remove(&canonical_source);
        result
    }

    fn inspect_items(
        &mut self,
        target: &TargetInfo,
        current_module_id: &str,
        segments: &[String],
        source: &Path,
        module_dir: &Path,
        items: &[Item],
    ) -> anyhow::Result<()> {
        self.register_items(target, segments, items);
        let source_normalized = self.normalize_path(source);
        let mut visitor = DependencyVisitor::new(
            current_module_id,
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
                    if existing == &canonical_source {
                        continue;
                    }
                    self.add_diagnostic(
                        "cfg-ambiguous-module",
                        format!("inline module '{child_id}' has ambiguous sources"),
                        Some(source),
                        Some(item_mod.span().start().line),
                    );
                    continue;
                }
                self.module_sources
                    .insert(child_id.clone(), canonical_source);
                self.register_module(target, &child_segments, source);
                let child_dir = module_dir.join(&child_name);
                self.inspect_items(
                    target,
                    &child_id,
                    &child_segments,
                    source,
                    &child_dir,
                    inline_items,
                )?;
                continue;
            }

            let line = item_mod.span().start().line;
            match resolve_module_file(module_dir, &child_name, &item_mod.attrs) {
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
        let mut names = items.iter().filter_map(item_name).collect::<BTreeSet<_>>();
        for item_use in items.iter().filter_map(|item| match item {
            Item::Use(item_use) => Some(item_use),
            _ => None,
        }) {
            for import in flatten_item_use(item_use) {
                names.insert(import.exposed_name);
            }
        }
        self.module_items
            .entry((target.root_id.clone(), segments.join("::")))
            .or_default()
            .extend(names);
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
        let mut reexports = BTreeMap::<(String, String), BTreeSet<String>>::new();
        let raw_dependencies = std::mem::take(&mut self.raw_dependencies);
        for raw in raw_dependencies
            .iter()
            .filter(|raw| raw.reexported_name.is_some())
        {
            match self.resolve_dependency(raw, &BTreeMap::new()) {
                Resolution::Module(target) if target != raw.source => {
                    reexports
                        .entry((
                            raw.source.clone(),
                            raw.reexported_name.clone().expect("re-export was filtered"),
                        ))
                        .or_default()
                        .insert(target.clone());
                    dependencies
                        .entry((raw.source.clone(), target.clone()))
                        .or_insert(Dependency {
                            source: raw.source.clone(),
                            target,
                            evidence: raw.evidence.clone(),
                        });
                }
                Resolution::Module(_) | Resolution::External | Resolution::LocalItem => {}
                Resolution::Opaque { .. } => unreachable!("re-export map starts empty"),
                Resolution::Unresolved(message) => self.record_unresolved(raw, message),
            }
        }
        for raw in raw_dependencies
            .iter()
            .filter(|raw| raw.reexported_name.is_none())
        {
            match self.resolve_dependency(raw, &reexports) {
                Resolution::Module(target) if target != raw.source => {
                    dependencies
                        .entry((raw.source.clone(), target.clone()))
                        .or_insert(Dependency {
                            source: raw.source.clone(),
                            target,
                            evidence: raw.evidence.clone(),
                        });
                }
                Resolution::Opaque {
                    targets,
                    via,
                    exported_name,
                } => {
                    for target in targets {
                        self.graph.opaque_reexports.insert(OpaqueReexport {
                            source: raw.source.clone(),
                            target,
                            via: via.clone(),
                            exported_name: exported_name.clone(),
                            evidence: raw.evidence.clone(),
                        });
                    }
                }
                Resolution::Module(_) | Resolution::External | Resolution::LocalItem => {}
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
        reexports: &BTreeMap<(String, String), BTreeSet<String>>,
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

        match segments[0].as_str() {
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
            first
                if target.external_aliases.contains(first) || STANDARD_CRATES.contains(&first) =>
            {
                return Resolution::External;
            }
            first => {
                let top_level_key = (raw.target_root.clone(), first.to_owned());
                if !self.module_paths.contains_key(&top_level_key) {
                    if raw.origin == DependencyOrigin::Path {
                        return Resolution::LocalItem;
                    }
                    if self
                        .module_items
                        .get(&(raw.target_root.clone(), String::new()))
                        .is_some_and(|items| items.contains(first))
                    {
                        return Resolution::LocalItem;
                    }
                    return Resolution::Unresolved(format!(
                        "could not resolve import root '{}' in target '{}'",
                        first, target.name
                    ));
                }
            }
        }
        candidate.extend(segments.iter().cloned());

        for length in (0..=candidate.len()).rev() {
            let relative = candidate[..length].join("::");
            if let Some(module) = self
                .module_paths
                .get(&(target_root.to_owned(), relative.clone()))
            {
                let remaining = &candidate[length..];
                if remaining.is_empty() {
                    return Resolution::Module(module.clone());
                }
                let exposed_name = &remaining[0];
                if let Some(targets) = reexports.get(&(module.clone(), exposed_name.clone())) {
                    return Resolution::Opaque {
                        targets: targets.iter().cloned().collect(),
                        via: module.clone(),
                        exported_name: exposed_name.clone(),
                    };
                }
                if let Some(targets) = reexports.get(&(module.clone(), "*".to_owned())) {
                    return Resolution::Opaque {
                        targets: targets.iter().cloned().collect(),
                        via: module.clone(),
                        exported_name: "*".to_owned(),
                    };
                }
                if self
                    .module_items
                    .get(&(target_root.to_owned(), relative))
                    .is_some_and(|items| items.contains(exposed_name))
                {
                    return Resolution::Module(module.clone());
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
            raw.evidence.expression, target.name
        ))
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
    Module(String),
    Opaque {
        targets: Vec<String>,
        via: String,
        exported_name: String,
    },
    External,
    LocalItem,
    Unresolved(String),
}

struct DependencyVisitor<'a> {
    source: &'a str,
    target_root: &'a str,
    current_segments: &'a [String],
    source_path: &'a str,
    strict: bool,
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
            dependencies: Vec::new(),
            diagnostics: BTreeSet::new(),
        }
    }

    fn push_dependency(
        &mut self,
        segments: Vec<String>,
        origin: DependencyOrigin,
        reexported_name: Option<String>,
        span: Span,
    ) {
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
                expression: segments.join("::"),
            },
            segments,
            origin,
            reexported_name,
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
    fn visit_item_mod(&mut self, _node: &'ast syn::ItemMod) {}

    fn visit_item_use(&mut self, node: &'ast ItemUse) {
        let is_reexport = !matches!(node.vis, Visibility::Inherited);
        for import in flatten_item_use(node) {
            let reexported_name = is_reexport.then_some(import.exposed_name);
            self.push_dependency(
                import.segments,
                DependencyOrigin::Use,
                reexported_name,
                node.span(),
            );
        }
    }

    fn visit_item_extern_crate(&mut self, node: &'ast ItemExternCrate) {
        self.push_dependency(
            vec![node.ident.to_string()],
            DependencyOrigin::Use,
            None,
            node.span(),
        );
    }

    fn visit_path(&mut self, node: &'ast SynPath) {
        let segments = path_segments(node);
        if segments.len() > 1
            || segments
                .first()
                .is_some_and(|segment| matches!(segment.as_str(), "crate" | "self" | "super"))
        {
            self.push_dependency(segments, DependencyOrigin::Path, None, node.span());
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

fn flatten_item_use(item_use: &ItemUse) -> Vec<UseImport> {
    let mut imports = Vec::new();
    flatten_use_tree(&item_use.tree, Vec::new(), &mut imports);
    imports
}

fn flatten_use_tree(tree: &UseTree, prefix: Vec<String>, imports: &mut Vec<UseImport>) {
    match tree {
        UseTree::Path(path) => {
            let mut next = prefix;
            next.push(path.ident.to_string());
            flatten_use_tree(&path.tree, next, imports);
        }
        UseTree::Name(name) => {
            let mut path = prefix;
            if name.ident != "self" {
                path.push(name.ident.to_string());
            }
            let exposed_name = path
                .last()
                .cloned()
                .expect("a use name has an exposed identifier");
            imports.push(UseImport {
                segments: path,
                exposed_name,
            });
        }
        UseTree::Rename(rename) => {
            let mut path = prefix;
            path.push(rename.ident.to_string());
            imports.push(UseImport {
                segments: path,
                exposed_name: rename.rename.to_string(),
            });
        }
        UseTree::Glob(_) => imports.push(UseImport {
            segments: prefix,
            exposed_name: "*".to_owned(),
        }),
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use_tree(item, prefix.clone(), imports);
            }
        }
    }
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
        Item::ExternCrate(item) => item
            .rename
            .as_ref()
            .map(|(_, rename)| rename.to_string())
            .or_else(|| Some(item.ident.to_string())),
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

fn resolve_module_file(
    module_dir: &Path,
    name: &str,
    attributes: &[Attribute],
) -> Result<(PathBuf, PathBuf), String> {
    if let Some(custom_path) = module_path_attribute(attributes)? {
        let source = module_dir.join(&custom_path);
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
        let imports = flatten_item_use(&item);
        assert_eq!(
            imports,
            vec![
                UseImport {
                    segments: vec!["crate".to_owned(), "a".to_owned()],
                    exposed_name: "a".to_owned(),
                },
                UseImport {
                    segments: vec!["crate".to_owned(), "a".to_owned(), "B".to_owned()],
                    exposed_name: "B".to_owned(),
                },
                UseImport {
                    segments: vec!["crate".to_owned(), "a".to_owned(), "c".to_owned()],
                    exposed_name: "*".to_owned(),
                },
            ]
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
