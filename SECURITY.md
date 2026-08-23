# Security Policy

## Supported Versions

The latest released minor version receives security fixes. Before a tagged
release, report issues against `main`.

## Reporting a Vulnerability

Use GitHub's private security advisory flow for
`nfma/hexagonal-architecture-validator`. Do not open a public issue containing
exploit details, secrets, or a malicious fixture. Include the affected version,
impact, reachable input, and a minimal reproduction when safe.

## System and Scope

The validator is a Rust CLI that analyzes untrusted Rust workspaces, source,
Cargo metadata, TOML configuration, module paths, imports, aliases and
re-exports to enforce declared architecture boundaries. Analysis, reporting,
package construction, release artifacts, and CI workflows are in scope.

## Threat Model and Trust Boundaries

Treat the target workspace, source text, manifests, configuration, symlinks,
paths, module attributes, names, Cargo metadata output, and CLI arguments as
attacker-controlled. Selecting a workspace authorizes bounded read-only
analysis; it never authorizes executing that workspace.

## Security Invariants

- Analysis must not execute project build scripts, procedural macros, binaries,
  tests, or arbitrary commands from the target workspace.
- Every resolved file and module must remain within the canonical selected
  workspace; traversal, symlink, inline-module and `#[path]` escapes fail closed.
- Unsupported, ambiguous, malformed, cyclic, missing, or unreadable analysis
  inputs must produce a visible failure rather than silently weakening a rule.
- Namespace, edition, alias, re-export, exemption and role resolution must not
  turn allowed syntax into false violations or hide a forbidden dependency.
- Parsing and graph traversal must be bounded and panic-free for attacker-
  controlled source and configuration.
- Human and JSON reports must agree on violations, exemptions, analysis failures
  and exit status without terminal-control injection or path disclosure beyond
  the selected workspace.
- Release and CI workflows must use immutable actions, verified dependencies,
  protected release inputs and smoke-tested artifacts.

## Reportable Findings and Severity Context

Report untrusted code execution, writes or network access during analysis,
workspace escapes, fail-open resolution, denial of service from unbounded input
or panics, hidden forbidden dependencies, materially false enforcement, report
contract mismatches, and release-integrity bypasses.

## Known Limitations

The validator performs static source analysis and does not prove runtime
architecture. The Semgrep gate rejects findings, scan degradation, unreviewed
parser warnings, and required-path coverage loss; its reviewed baseline is not
an exemption from validator correctness.

See [the Semgrep gate runbook](docs/SEMGREP.md) for scan scope and baseline
review procedure.
