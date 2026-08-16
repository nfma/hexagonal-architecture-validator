# hexagonal-architecture-validator

`hav` is a deterministic Rust CLI for checking declared dependency boundaries.
It discovers Cargo workspace targets and Rust modules, builds a normalized static
dependency graph, then evaluates explicit role-based rules. The built-in
hexagonal preset catches dependencies from core, application, or port code to
concrete adapters and keeps narrow exceptions explicit and auditable.

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

Exit codes are part of the CLI contract:

| Code | Meaning |
| ---: | --- |
| `0` | Analysis completed and no architectural violations were found. |
| `1` | Analysis completed and architectural violations were found. |
| `2` | Configuration, discovery, parsing, resolution, or reporting failed. |

## Configuration

Configuration is explicit and versioned. Roles map module IDs or normalized
workspace-relative source paths to architectural intent. Every declared role
must match at least one module, except roles the hexagonal preset mandates but
the project does not populate, which produce a non-fatal notice. Forbidden rules
match dependencies by source and target roles. Allowed rules must name the exact
forbidden IDs they exempt, and every applied exemption appears in JSON and text
reports.

```toml
version = 1
preset = "hexagonal"

[analysis]
strict = true
detect_cycles = true

[[roles]]
id = "core"
paths = ["^src/core(?:/|\\.rs$)"]

[[roles]]
id = "application"
paths = ["^src/application(?:/|\\.rs$)"]

[[roles]]
id = "port"
paths = ["^src/ports(?:/|\\.rs$)"]

[[roles]]
id = "adapter"
paths = ["^src/adapters(?:/|\\.rs$)"]

[[roles]]
id = "composition-root"
paths = ["^src/main\\.rs$", "^src/bin/[^/]+\\.rs$"]
```

See [Configuration](docs/CONFIGURATION.md) for the complete schema and preset
contract.

## Analysis model

`hav` invokes `cargo metadata --format-version 1 --no-deps --offline`, excludes
custom build-script targets, parses Rust source with `syn`, discovers file and
inline modules, and resolves local plus workspace-crate imports. Findings,
paths, modules, edges, and diagnostics are sorted deterministically.

Macro expansion and cfg evaluation are deliberately outside the first release.
Strict analysis defaults on; `strict = false` is the explicit opt-out for
unsupported item-position macros. Bare and qualified `include!` forms always
fail analysis. Repeated cfg declarations coalesce only when they resolve to the
same canonical file, and module sources cannot escape the Cargo workspace via
absolute paths, traversal, or symlinks. All reports list the remaining
false-positive and false-negative risks. See
[Analysis and limitations](docs/ANALYSIS.md).

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
