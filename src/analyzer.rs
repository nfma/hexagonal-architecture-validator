use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use cargo_metadata::{MetadataCommand, Package, Target};
use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Item, ItemExternCrate, ItemMacro, ItemUse, Macro, Path as SynPath, UseTree};

use crate::model::{AnalysisDiagnostic, Dependency, DependencyGraph, Evidence, Module};

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
    evidence: Evidence,
}

#[derive(Default)]
struct ModuleImports {
    bindings: BTreeMap<String, Vec<String>>,
    glob_prefixes: BTreeSet<Vec<String>>,
}

struct Analyzer {
    workspace_root: PathBuf,
    strict: bool,
    targets: BTreeMap<String, TargetInfo>,
    graph: DependencyGraph,
    raw_dependencies: Vec<RawDependency>,
    module_paths: BTreeMap<(String, String), String>,
    module_items: BTreeMap<(String, String), BTreeSet<String>>,
    module_imports: BTreeMap<(String, String), ModuleImports>,
    visited_modules: BTreeSet<String>,
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
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
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
        module_imports: BTreeMap::new(),
        visited_modules: BTreeSet::new(),
    };

    let targets = analyzer.targets.values().cloned().collect::<Vec<_>>();
    for target in targets {
        let module_dir = target
            .source
            .parent()
            .context("Cargo target source has no parent directory")?
            .to_path_buf();
        analyzer.discover_file_module(&target, Vec::new(), &target.source, &module_dir)?;
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
                .map(|(_, root, _, _)| (package.clone(), root.clone()))
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
                let alias = dependency
                    .rename
                    .as_deref()
                    .unwrap_or(dependency.name.as_str())
                    .replace('-', "_");
                if let Some(root) = library_roots.get(dependency.name.as_str()) {
                    workspace_aliases.insert(alias, root.clone());
                } else {
                    external_aliases.insert(alias);
                }
            }
            if !is_library {
                if let Some(library_root) = library_roots.get(package) {
                    let library_name = package.replace('-', "_");
                    workspace_aliases.insert(library_name, library_root.clone());
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
    ) -> anyhow::Result<()> {
        let module_id = module_id(&target.root_id, &segments);
        if !self.visited_modules.insert(module_id.clone()) {
            self.add_diagnostic(
                "duplicate-module",
                format!("module '{module_id}' was discovered more than once"),
                Some(source),
                None,
            );
            return Ok(());
        }

        let contents = match fs::read_to_string(source) {
            Ok(contents) => contents,
            Err(error) => {
                self.add_diagnostic(
                    "source-read-failed",
                    format!("could not read Rust source: {error}"),
                    Some(source),
                    None,
                );
                return Ok(());
            }
        };
        let file = match syn::parse_file(&contents) {
            Ok(file) => file,
            Err(error) => {
                self.add_diagnostic(
                    "parse-failed",
                    error.to_string(),
                    Some(source),
                    Some(error.span().start().line),
                );
                return Ok(());
            }
        };

        self.register_module(target, &segments, source);
        self.inspect_items(target, &segments, source, module_dir, false, &file.items)
    }

    fn inspect_items(
        &mut self,
        target: &TargetInfo,
        segments: &[String],
        source: &Path,
        module_dir: &Path,
        inside_inline_module: bool,
        items: &[Item],
    ) -> anyhow::Result<()> {
        self.register_items(target, segments, items);
        self.register_imports(target, segments, items);
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
                if !self.visited_modules.insert(child_id.clone()) {
                    self.add_diagnostic(
                        "duplicate-module",
                        format!("module '{child_id}' was discovered more than once"),
                        Some(source),
                        Some(item_mod.span().start().line),
                    );
                    continue;
                }
                self.register_module(target, &child_segments, source);
                let child_dir = module_dir.join(&child_name);
                self.inspect_items(
                    target,
                    &child_segments,
                    source,
                    &child_dir,
                    true,
                    inline_items,
                )?;
                continue;
            }

            let line = item_mod.span().start().line;
            match resolve_module_file(
                source,
                module_dir,
                inside_inline_module,
                &child_name,
                &item_mod.attrs,
            ) {
                Ok((child_source, child_dir)) => {
                    self.discover_file_module(target, child_segments, &child_source, &child_dir)?;
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
        let names = items.iter().filter_map(item_name).collect::<BTreeSet<_>>();
        self.module_items
            .insert((target.root_id.clone(), segments.join("::")), names);
    }

    fn register_imports(&mut self, target: &TargetInfo, segments: &[String], items: &[Item]) {
        let mut imports = ModuleImports::default();
        for item_use in items.iter().filter_map(|item| match item {
            Item::Use(item_use) => Some(item_use),
            _ => None,
        }) {
            collect_use_bindings(&item_use.tree, Vec::new(), &mut imports.bindings);
            collect_glob_prefixes(&item_use.tree, Vec::new(), &mut imports.glob_prefixes);
        }
        self.module_imports
            .insert((target.root_id.clone(), segments.join("::")), imports);
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
        let raw_dependencies = std::mem::take(&mut self.raw_dependencies);
        for raw in raw_dependencies {
            match self.resolve_dependency(&raw) {
                Resolution::Module(target) if target != raw.source => {
                    dependencies
                        .entry((raw.source.clone(), target.clone()))
                        .or_insert(Dependency {
                            source: raw.source,
                            target,
                            evidence: raw.evidence,
                        });
                }
                Resolution::Module(_) | Resolution::External | Resolution::LocalItem => {}
                Resolution::Unresolved(message) if raw.origin == DependencyOrigin::Use => {
                    self.graph.diagnostics.insert(AnalysisDiagnostic {
                        code: "unresolved-import".to_owned(),
                        message,
                        path: Some(raw.evidence.path),
                        line: Some(raw.evidence.line),
                    });
                }
                Resolution::Unresolved(_) => {}
            }
        }
        self.graph.dependencies = dependencies.into_values().collect();
    }

    fn resolve_dependency(&self, raw: &RawDependency) -> Resolution {
        if raw.segments.is_empty() {
            return Resolution::LocalItem;
        }
        let segments = match self.expand_module_bindings(raw) {
            Ok(segments) => segments,
            Err(message) => return Resolution::Unresolved(message),
        };
        let resolution = self.resolve_segments(raw, &segments);
        if !matches!(resolution, Resolution::Unresolved(_)) {
            return resolution;
        }
        match self.expand_glob_import(raw, &segments) {
            Ok(Some(glob_expanded)) => self.resolve_segments(raw, &glob_expanded),
            Ok(None) => resolution,
            Err(message) => Resolution::Unresolved(message),
        }
    }

    fn resolve_segments(&self, raw: &RawDependency, segments: &[String]) -> Resolution {
        let target = self
            .targets
            .get(&raw.target_root)
            .expect("raw dependency refers to a known target");
        if let Some(resolution) = self.resolve_explicit_path(raw, segments) {
            return self.finish_resolution(resolution);
        }
        if let Some(resolution) = self.resolve_current_module_scope(raw, segments) {
            return self.finish_resolution(resolution);
        }
        if let Some(resolution) = self.resolve_crate_alias(target, segments) {
            return self.finish_resolution(resolution);
        }
        if is_standard_crate(&segments[0]) {
            return Resolution::External;
        }
        if let Some(resolution) = self.resolve_top_level_module(raw, segments) {
            return self.finish_resolution(resolution);
        }
        if is_standard_prelude_name(&segments[0]) {
            return Resolution::External;
        }
        if self.is_crate_root_item(raw, &segments[0]) {
            return Resolution::LocalItem;
        }
        Resolution::Unresolved(format!(
            "could not resolve import root '{}' in target '{}'",
            segments[0], target.name
        ))
    }

    fn expand_module_bindings(&self, raw: &RawDependency) -> Result<Vec<String>, String> {
        let import_key = (raw.target_root.clone(), raw.current_segments.join("::"));
        let Some(imports) = self.module_imports.get(&import_key) else {
            return Ok(raw.segments.clone());
        };
        expand_bindings(
            &imports.bindings,
            raw.segments.clone(),
            &raw.evidence.expression,
        )
    }

    fn expand_glob_import(
        &self,
        raw: &RawDependency,
        segments: &[String],
    ) -> Result<Option<Vec<String>>, String> {
        if segments
            .first()
            .is_some_and(|first| matches!(first.as_str(), "crate" | "self" | "super"))
        {
            return Ok(None);
        }
        let import_key = (raw.target_root.clone(), raw.current_segments.join("::"));
        let Some(imports) = self.module_imports.get(&import_key) else {
            return Ok(None);
        };

        for prefix in &imports.glob_prefixes {
            let expanded_prefix =
                expand_bindings(&imports.bindings, prefix.clone(), &raw.evidence.expression)?;
            let Resolution::Module(module) = self.resolve_segments(raw, &expanded_prefix) else {
                continue;
            };
            if self.module_contains_name(&module, &segments[0]) {
                let mut glob_expanded = expanded_prefix;
                glob_expanded.extend_from_slice(segments);
                return Ok(Some(glob_expanded));
            }
        }
        Ok(None)
    }

    fn module_contains_name(&self, module_id: &str, name: &str) -> bool {
        let Some(((target_root, relative), _)) = self
            .module_paths
            .iter()
            .find(|(_, candidate)| candidate.as_str() == module_id)
        else {
            return false;
        };
        if self
            .module_items
            .get(&(target_root.clone(), relative.clone()))
            .is_some_and(|items| items.contains(name))
            || self
                .module_imports
                .get(&(target_root.clone(), relative.clone()))
                .is_some_and(|imports| imports.bindings.contains_key(name))
        {
            return true;
        }
        let child = if relative.is_empty() {
            name.to_owned()
        } else {
            format!("{relative}::{name}")
        };
        self.module_paths
            .contains_key(&(target_root.clone(), child))
    }

    fn resolve_top_level_module(
        &self,
        raw: &RawDependency,
        segments: &[String],
    ) -> Option<ResolutionStep> {
        self.module_paths
            .contains_key(&(raw.target_root.clone(), segments[0].clone()))
            .then(|| ResolutionStep::Search {
                target_root: raw.target_root.clone(),
                candidate: segments.to_vec(),
            })
    }

    fn is_crate_root_item(&self, raw: &RawDependency, name: &str) -> bool {
        self.module_items
            .get(&(raw.target_root.clone(), String::new()))
            .is_some_and(|items| items.contains(name))
    }

    fn resolve_explicit_path(
        &self,
        raw: &RawDependency,
        segments: &[String],
    ) -> Option<ResolutionStep> {
        let mut candidate = raw.current_segments.clone();
        let remaining = match segments[0].as_str() {
            "crate" => {
                candidate.clear();
                &segments[1..]
            }
            "self" => &segments[1..],
            "super" => {
                let mut remaining = segments;
                while remaining.first().is_some_and(|segment| segment == "super") {
                    if candidate.pop().is_none() {
                        return Some(ResolutionStep::Complete(Resolution::Unresolved(format!(
                            "'{}' traverses beyond the crate root",
                            raw.evidence.expression
                        ))));
                    }
                    remaining = &remaining[1..];
                }
                remaining
            }
            _ => return None,
        };
        candidate.extend_from_slice(remaining);
        Some(ResolutionStep::Search {
            target_root: raw.target_root.clone(),
            candidate,
        })
    }

    fn resolve_current_module_scope(
        &self,
        raw: &RawDependency,
        segments: &[String],
    ) -> Option<ResolutionStep> {
        let current_key = (raw.target_root.clone(), raw.current_segments.join("::"));
        let mut child_segments = raw.current_segments.clone();
        child_segments.push(segments[0].clone());
        let child_key = (raw.target_root.clone(), child_segments.join("::"));
        if self.module_paths.contains_key(&child_key) {
            let mut candidate = raw.current_segments.clone();
            candidate.extend_from_slice(segments);
            return Some(ResolutionStep::Search {
                target_root: raw.target_root.clone(),
                candidate,
            });
        }
        self.module_items
            .get(&current_key)
            .is_some_and(|items| items.contains(&segments[0]))
            .then_some(ResolutionStep::Complete(Resolution::LocalItem))
    }

    fn resolve_crate_alias(
        &self,
        target: &TargetInfo,
        segments: &[String],
    ) -> Option<ResolutionStep> {
        let first = &segments[0];
        if let Some(target_root) = target.workspace_aliases.get(first) {
            return Some(ResolutionStep::Search {
                target_root: target_root.clone(),
                candidate: segments[1..].to_vec(),
            });
        }
        target
            .external_aliases
            .contains(first)
            .then_some(ResolutionStep::Complete(Resolution::External))
    }

    fn finish_resolution(&self, resolution: ResolutionStep) -> Resolution {
        let (target_root, candidate) = match resolution {
            ResolutionStep::Search {
                target_root,
                candidate,
            } => (target_root, candidate),
            ResolutionStep::Complete(resolution) => return resolution,
        };
        for length in (0..=candidate.len()).rev() {
            let relative = candidate[..length].join("::");
            if let Some(module) = self.module_paths.get(&(target_root.clone(), relative)) {
                return Resolution::Module(module.clone());
            }
        }
        Resolution::LocalItem
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
    External,
    LocalItem,
    Unresolved(String),
}

enum ResolutionStep {
    Search {
        target_root: String,
        candidate: Vec<String>,
    },
    Complete(Resolution),
}

fn is_standard_crate(name: &str) -> bool {
    STANDARD_CRATES.contains(&name)
}

fn is_standard_prelude_name(name: &str) -> bool {
    STANDARD_PRELUDE_NAMES.contains(&name)
}

struct DependencyVisitor<'a> {
    source: &'a str,
    target_root: &'a str,
    current_segments: &'a [String],
    source_path: &'a str,
    strict: bool,
    scoped_imports: Vec<BTreeMap<String, Vec<String>>>,
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
            scoped_imports: Vec::new(),
            dependencies: Vec::new(),
            diagnostics: BTreeSet::new(),
        }
    }

    fn push_dependency(&mut self, segments: Vec<String>, origin: DependencyOrigin, span: Span) {
        if segments.is_empty() {
            return;
        }
        let expression = segments.join("::");
        let segments = self.expand_scoped_imports(segments);
        self.dependencies.push(RawDependency {
            source: self.source.to_owned(),
            target_root: self.target_root.to_owned(),
            current_segments: self.current_segments.to_vec(),
            evidence: Evidence {
                path: self.source_path.to_owned(),
                line: span.start().line,
                expression,
            },
            segments,
            origin,
        });
    }

    fn expand_scoped_imports(&self, mut segments: Vec<String>) -> Vec<String> {
        let mut expanded = BTreeSet::new();
        while let Some(first) = segments.first() {
            let Some(imported) = self
                .scoped_imports
                .iter()
                .rev()
                .find_map(|scope| scope.get(first))
            else {
                break;
            };
            if !expanded.insert(first.clone()) {
                break;
            }
            let mut replacement = imported.clone();
            replacement.extend(segments.into_iter().skip(1));
            segments = replacement;
        }
        segments
    }

    fn register_scoped_imports(&mut self, tree: &UseTree) {
        if self.scoped_imports.is_empty() {
            return;
        }
        let mut bindings = BTreeMap::new();
        collect_use_bindings(tree, Vec::new(), &mut bindings);
        let expanded = bindings
            .into_iter()
            .map(|(name, path)| (name, self.expand_scoped_imports(path)))
            .collect::<Vec<_>>();
        self.scoped_imports
            .last_mut()
            .expect("a scoped import frame exists")
            .extend(expanded);
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
        let mut paths = Vec::new();
        flatten_use_tree(&node.tree, Vec::new(), &mut paths);
        for path in paths {
            self.push_dependency(path, DependencyOrigin::Use, node.span());
        }
        self.register_scoped_imports(&node.tree);
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        self.scoped_imports.push(BTreeMap::new());
        visit::visit_block(self, node);
        self.scoped_imports
            .pop()
            .expect("the scoped import frame was pushed");
    }

    fn visit_item_extern_crate(&mut self, node: &'ast ItemExternCrate) {
        self.push_dependency(
            vec![node.ident.to_string()],
            DependencyOrigin::Use,
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
            self.push_dependency(segments, DependencyOrigin::Path, node.span());
        }
        visit::visit_path(self, node);
    }

    fn visit_item_macro(&mut self, node: &'ast ItemMacro) {
        if node
            .mac
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "include")
        {
            self.record_unsupported_macro(&node.mac, "unsupported-include");
        } else if self.strict {
            self.record_unsupported_macro(&node.mac, "unsupported-item-macro");
        }
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        if node
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "include")
        {
            self.record_unsupported_macro(node, "unsupported-include");
        }
        visit::visit_macro(self, node);
    }
}

fn flatten_use_tree(tree: &UseTree, prefix: Vec<String>, paths: &mut Vec<Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            let mut next = prefix;
            next.push(path.ident.to_string());
            flatten_use_tree(&path.tree, next, paths);
        }
        UseTree::Name(name) => {
            let mut path = prefix;
            if name.ident != "self" {
                path.push(name.ident.to_string());
            }
            paths.push(path);
        }
        UseTree::Rename(rename) => {
            let mut path = prefix;
            path.push(rename.ident.to_string());
            paths.push(path);
        }
        UseTree::Glob(_) => paths.push(prefix),
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use_tree(item, prefix.clone(), paths);
            }
        }
    }
}

fn collect_use_bindings(
    tree: &UseTree,
    prefix: Vec<String>,
    bindings: &mut BTreeMap<String, Vec<String>>,
) {
    match tree {
        UseTree::Path(path) => {
            let mut next = prefix;
            next.push(path.ident.to_string());
            collect_use_bindings(&path.tree, next, bindings);
        }
        UseTree::Name(name) => {
            let mut path = prefix;
            let binding = if name.ident == "self" {
                let Some(binding) = path.last().cloned() else {
                    return;
                };
                binding
            } else {
                let binding = name.ident.to_string();
                path.push(binding.clone());
                binding
            };
            if path.len() != 1 || path[0] != binding {
                bindings.insert(binding, path);
            }
        }
        UseTree::Rename(rename) => {
            let mut path = prefix;
            path.push(rename.ident.to_string());
            bindings.insert(rename.rename.to_string(), path);
        }
        UseTree::Glob(_) => {}
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_bindings(item, prefix.clone(), bindings);
            }
        }
    }
}

fn collect_glob_prefixes(
    tree: &UseTree,
    prefix: Vec<String>,
    prefixes: &mut BTreeSet<Vec<String>>,
) {
    match tree {
        UseTree::Path(path) => {
            let mut next = prefix;
            next.push(path.ident.to_string());
            collect_glob_prefixes(&path.tree, next, prefixes);
        }
        UseTree::Glob(_) => {
            prefixes.insert(prefix);
        }
        UseTree::Group(group) => {
            for item in &group.items {
                collect_glob_prefixes(item, prefix.clone(), prefixes);
            }
        }
        UseTree::Name(_) | UseTree::Rename(_) => {}
    }
}

fn expand_bindings(
    bindings: &BTreeMap<String, Vec<String>>,
    mut segments: Vec<String>,
    expression: &str,
) -> Result<Vec<String>, String> {
    let mut expanded = BTreeSet::new();
    while let Some(first) = segments.first() {
        if matches!(first.as_str(), "crate" | "self" | "super") {
            break;
        }
        let Some(imported) = bindings.get(first) else {
            break;
        };
        if !expanded.insert(first.clone()) {
            return Err(format!("import '{expression}' contains a cyclic alias"));
        }
        let mut replacement = imported.clone();
        replacement.extend(segments.into_iter().skip(1));
        segments = replacement;
    }
    Ok(segments)
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
    declaring_source: &Path,
    module_dir: &Path,
    inside_inline_module: bool,
    name: &str,
    attributes: &[Attribute],
) -> Result<(PathBuf, PathBuf), String> {
    if let Some(custom_path) = module_path_attribute(attributes)? {
        let base = if inside_inline_module {
            module_dir
        } else {
            declaring_source.parent().unwrap_or(module_dir)
        };
        let source = base.join(&custom_path);
        if !source.is_file() {
            return Err(format!(
                "module '{name}' points to missing #[path] file '{}'",
                custom_path.display()
            ));
        }
        let child_dir = source
            .parent()
            .unwrap_or(module_dir)
            .join(source.file_stem().unwrap_or_default());
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
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, Vec::new(), &mut paths);
        assert_eq!(
            paths,
            vec![
                vec!["crate", "a"],
                vec!["crate", "a", "B"],
                vec!["crate", "a", "c"],
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
