# hexagonal-architecture-validator

`hav` is a deterministic Rust CLI for checking declared dependency boundaries.
It discovers Cargo workspace targets and Rust modules, builds a normalized static
dependency graph, then evaluates explicit role-based rules. The built-in
hexagonal preset catches dependencies from core, application, or port code to
concrete adapters and keeps composition-root wiring explicit.

The result is architecture evidence, not a claim that folder names or static
dependencies prove architectural quality.

## Quick start

Download and verify a release using the commands in
[Installation](docs/INSTALLATION.md), then add a `hav.toml` based on
[the hexagonal example](examples/hexagonal.hav.toml):

```console
hav check --root .
```

Machine-readable output is stable and versioned:

```console
hav check --root . --format json
```

JSON always uses the same `ValidationReport` document, including configuration
and discovery failures. Its `outcome` is exactly `passed`, `violations`, or
`analysis-failure`. Text reports for all three outcomes are written to stdout.

Exit codes are part of the CLI contract:

| Code | Meaning |
| ---: | --- |
| `0` | Analysis completed and no architectural violations were found. |
| `1` | Analysis completed and architectural violations were found. |
| `2` | Configuration, discovery, parsing, resolution, or reporting failed. |

## Configuration

Configuration is explicit and versioned. Roles map module IDs or normalized
workspace-relative source paths to architectural intent. Forbidden rules match
dependencies by source and target roles. Allowed rules are named, explicit
exceptions evaluated before forbidden rules.

```toml
version = 1
preset = "hexagonal"

[analysis]
strict = true
detect_cycles = true

# Paths are relative to the Cargo workspace root.
[[roles]]
id = "core"
paths = ["^(?:crates/[^/]+/)?src/core(?:/|\\.rs$)"]

[[roles]]
id = "adapter"
paths = ["^(?:crates/[^/]+/)?src/adapters(?:/|\\.rs$)"]
```

See [Configuration](docs/CONFIGURATION.md) for the complete schema and preset
contract.

## Analysis model

`hav` invokes `cargo metadata --format-version 1 --no-deps --offline` for
`--root/Cargo.toml` unless `--manifest-path` is supplied, excludes
custom build-script targets, parses Rust source with `syn`, discovers file and
inline modules, and resolves local plus workspace-crate imports. Findings,
paths, modules, edges, and diagnostics are sorted deterministically.

Macro expansion and cfg evaluation are deliberately outside the first release.
`include!` is an analysis error; `--strict` also makes item-position macro
invocations analysis errors. JSON reports that reach rule evaluation list the
remaining false-positive and false-negative risks. Text reports do not render
these limitations, and failures returned before evaluation use an empty JSON
limitations list. See [Analysis and limitations](docs/ANALYSIS.md).

## Development

The minimum supported Rust version is 1.86.

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo deny check
```

Release artifacts and provenance are described in
[Release process](docs/RELEASE.md).

## License

MIT. See [LICENSE](LICENSE).
