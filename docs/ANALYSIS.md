# Analysis and limitations

## Mechanism decision

The first release uses Cargo metadata v1 for workspace/package/target discovery
and `syn` for stable-toolchain syntax parsing.

- Cargo documents `cargo metadata --format-version 1` as the versioned package
  graph interface and `--no-deps` as workspace-only output that does not fetch
  dependencies. `hav` additionally passes `--offline` so analysis cannot use
  the network: [Cargo metadata documentation](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html).
- `syn::parse_file` parses complete Rust source files on stable Rust and exposes
  spans used for deterministic evidence:
  [`syn::parse_file`](https://docs.rs/syn/latest/syn/fn.parse_file.html).
- rustdoc JSON was rejected for v0.1 because its output mode remains
  nightly/unstable:
  [rustdoc unstable JSON output](https://doc.rust-lang.org/nightly/rustdoc/unstable-features.html#json-output).
- rust-analyzer provides richer semantic resolution, but its internal library
  surface is a large compiler front end optimized for IDE analysis. Taking that
  dependency would significantly increase build size and couple v0.1 to an
  evolving internal API:
  [rust-analyzer repository](https://github.com/rust-lang/rust-analyzer).
- Existing `cargo-modules` demonstrates that module dependency and cycle
  analysis is useful, but its CLI graph is not the versioned role/rule/report
  contract required here:
  [cargo-modules repository](https://github.com/regexident/cargo-modules).

The rule vocabulary follows dependency-cruiser's named `from`/`to` dependency
conditions and explicit unresolved/circular concepts, while intentionally using
a smaller role-based Rust schema:
[dependency-cruiser rules reference](https://github.com/sverweij/dependency-cruiser/blob/main/doc/rules-reference.md).

## What is analyzed

- Every non-build-script Rust target in every Cargo workspace package.
- File modules (`mod name;`), `name.rs`, `name/mod.rs`, inline modules, and
  literal `#[path = "..."]` modules.
- `use`, `pub use`, `extern crate`, and syntactic qualified paths. Lexical alias
  chains retain their terminal module. Public re-export routes are recorded,
  and a route that could cross a forbidden boundary fails closed because v0.1
  does not model the complete visibility graph.
- Local `crate`, `self`, and `super` paths.
- Renamed and unrenamed direct dependencies between workspace library crates.
- Module-level dependency cycles.

Cargo discovery runs after dependencies are installed, offline, and without
repository writes by the validator. Analysis only reads manifests and source.

## Determinism

- Paths use `/`, are canonicalized for containment, rendered workspace-relative,
  and contain no timestamps.
- Modules, dependencies, findings, diagnostics, roles, and cycle members use
  ordered collections and stable sorting.
- Multiple references between the same source and target module become one
  edge with the earliest sorted source evidence.
- JSON has `schema_version = 1`; all top-level report fields are emitted in a
  fixed structure.

Every JSON result is a `ValidationReport` with one of three outcomes: `passed`,
`violations`, or `analysis-failure`. Configuration and discovery failures use
the same report shape with empty module, dependency, finding, exemption, and
limitation collections and one entry in `analysis_errors`.

## Explicit limitations

These limitations can create false positives or false negatives. Reports that
reach evaluation include this list in JSON output; text output does not render
it. Configuration, discovery, and other failures returned before evaluation
use the full JSON report shape with an empty `limitations` list.

- `cfg` predicates and Cargo feature selection are not evaluated. Every repeated
  inline body for one module is analyzed, while repeated file declarations that
  resolve to the same canonical file coalesce. If the same module resolves to
  different files across syntactic branches, analysis fails with
  `cfg-ambiguous-module` rather than choosing a branch. A conditional
  `#[cfg_attr(..., path = ...)]` fails closed as `unresolved-module`.
- Declarative and procedural macros, derives, and attribute macros are not
  expanded. Generated modules or imports can therefore be missed.
- `include!` always fails analysis. With strict analysis, other item-position
  macro invocations also fail instead of being silently trusted.
- Qualified-path resolution is module-oriented. Method calls, dynamic dispatch,
  trait implementation semantics, values constructed through reflection, and
  runtime service lookup do not create edges.
- Locally shadowed names, including types, generic parameters, and block-local
  items, take precedence over workspace-crate names. Block-local module bodies
  are not analyzed. `use` declarations do not observe block-local module
  shadowing: when the leading segment names a declared workspace crate, the
  import resolves to that crate and may produce a forbidden-edge finding;
  otherwise it fails closed as `unresolved-import`.
- External crates are recognized from Cargo dependency declarations but are not
  parsed. v0.1 rules validate workspace modules and workspace-crate edges.
- Literal `#[path]` is supported and resolves from the declaring file or inline
  module directory, matching rustc. Its child modules resolve from the selected
  path file's parent directory. The canonical source must remain inside the Cargo
  workspace; absolute paths, `..` traversal, and symlinks cannot escape it.
  Conditional path attributes fail closed, and macro-computed paths are not Rust
  syntax.
- Build scripts are excluded. Generated source in `OUT_DIR` is not analyzed.
- A pass means no violations were found in this declared static model. It does
  not decide whether ports are meaningful, business logic belongs in the core,
  adapters translate correctly, or the chosen boundaries fit the product.

## Stable analysis diagnostic codes

| Code | Meaning |
| --- | --- |
| `configuration-or-analysis-error` | Configuration loading or another top-level analysis failure prevented evaluation. |
| `cfg-ambiguous-module` | Repeated declarations of one module resolve to different canonical files. |
| `module-outside-workspace` | A module source resolves outside the canonical Cargo workspace root. |
| `opaque-reexport` | A dependency route through a public re-export could cross a configured forbidden rule that v0.1 cannot follow completely. |
| `parse-failed` | A discovered Rust source file cannot be parsed by `syn`. |
| `recursive-module-source` | A module recursively resolves to a canonical source already being inspected. |
| `role-matched-no-modules` | A declared role matches no discovered module. |
| `source-read-failed` | A source cannot be canonicalized or read. |
| `unresolved-import` | A `use` or qualified path cannot be resolved to a known item or module. |
| `unresolved-module` | A `mod` declaration has no valid source, uses a conditional path attribute, or is ambiguous. |
| `unsupported-include` | Any bare or qualified `include!` invocation requires unsupported expansion. |
| `unsupported-item-macro` | Strict analysis encountered another item-position macro it cannot expand. |

Every diagnostic in this table fails with exit code 2 and cannot be reported as
a clean architectural pass.
