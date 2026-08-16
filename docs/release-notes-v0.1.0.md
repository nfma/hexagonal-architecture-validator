# hexagonal-architecture-validator v0.1.0

First release of `hav`, a deterministic Rust dependency-boundary validator.

## Included

- Cargo workspace, target, file-module, inline-module, and workspace-crate
  discovery.
- Versioned role/rule config with a documented hexagonal preset and explicit
  composition-root exceptions.
- Forbidden-edge and module-cycle findings with stable evidence.
- Human-readable and schema-version-1 JSON reports.
- Distinct success (`0`), violation (`1`), and analysis/configuration (`2`)
  exits.
- Checksummed macOS Apple Silicon and Linux x86-64 archives with GitHub build
  provenance attestations.

## Parser and resolution limitations

- `cfg` and Cargo features are not evaluated; syntactically present branches
  are analyzed together.
- Macros, derives, and attribute macros are not expanded. `include!` is fatal;
  strict mode also rejects item-position macros.
- External crates and build-script generated source are not parsed.
- Method calls, dynamic dispatch, and runtime relationships do not create
  dependency edges.
- A pass is evidence about declared static dependencies, not proof that ports,
  adapters, business logic, or product boundaries are semantically correct.
