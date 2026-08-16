# Configuration

`hav` reads `hav.toml` from `--root` by default. `--config` accepts an absolute
path or a path relative to `--root`. Unknown fields and unsupported versions are
errors.

## Top-level schema

```toml
version = 1
preset = "hexagonal" # optional

[analysis]
strict = true        # optional, default true
detect_cycles = true # optional, default true
```

- `version` must be `1`.
- `preset` currently accepts `hexagonal`.
- `analysis.strict` makes item-position macros that cannot be expanded fatal.
  It defaults to `true`; `strict = false` is the explicit opt-out. `include!`
  and unresolved source modules remain fatal in every mode.
- `analysis.detect_cycles` emits one deterministic `no-cycles` finding for
  each strongly connected component containing more than one module.

The CLI `--strict` flag enables strict analysis even when a config explicitly
opts out.

## Roles

Every configuration declares at least one role:

```toml
[[roles]]
id = "core"
description = "Domain behavior" # optional
modules = ["::domain(?:$|::)"]  # optional Rust regular expressions
paths = ["^src/domain/"]        # optional Rust regular expressions
```

Role and rule IDs match `^[a-z][a-z0-9-]*$`. At least one `modules` or `paths`
pattern is required. A module may have multiple roles; matching any configured
pattern assigns the role. Every declared role must match at least one discovered
module. An unmatched role fails analysis with `role-matched-no-modules`.
Unclassified modules remain in the graph but do not match role rules.

Module IDs are stable strings of the form:

```text
package::kind(target)::nested::module
```

For example, package `orders` with library target `orders` has root module
`orders::lib(orders)` and child `orders::lib(orders)::domain`. Paths use `/`
separators and are relative to the canonical Cargo workspace root. A literal
`#[path]` that resolves outside that root, including through `..`, an absolute
path, or a symlink, fails analysis.

## Forbidden and allowed rules

Rules use dependency-cruiser-inspired `from` and `to` conditions over declared
roles:

```toml
[[forbidden]]
id = "core-must-not-depend-on-adapter"
description = "Core policy must not know concrete adapters"
from = ["core"]
to = ["adapter"]

[[allowed]]
id = "adapter-startup-hook"
description = "One adapter may invoke startup wiring"
from = ["adapter"]
to = ["composition-root"]
exempts = ["adapters-must-not-depend-on-composition-root"]
```

For each dependency edge, `hav`:

1. classifies its source and target modules;
2. finds every matching forbidden rule;
3. suppresses only those forbidden rules explicitly named by a matching
   allowed rule's `exempts` list;
4. emits findings for the remaining forbidden matches and audit records for
   every applied exemption.

Allowed rules are exceptions, not a global allowlist. `exempts` is required and
non-empty. Every entry must name a known forbidden rule, including preset rules;
unknown IDs, self-references, and allowed-rule IDs are configuration errors.
Allowed exceptions apply only to dependency rules and never suppress
`no-cycles`. Give exception roles narrow path or module patterns. Rule IDs are
globally unique across forbidden, allowed, and preset rules.

JSON always contains top-level `exemptions` and `summary.exemptions`, including
when both are empty. Text reports render each applied exemption as
`allowed[exception-id] exempted forbidden[rule-id] ...` under both Passed and
Violations outcomes. If every forbidden match is exempted, the exit code is `0`.

## Hexagonal preset

The preset requires roles named `core`, `application`, `port`, `adapter`, and
`composition-root`. It adds these stable rules:

| Rule ID | From | To |
| --- | --- | --- |
| `core-must-not-depend-on-adapters` | core | adapter |
| `core-must-not-depend-on-composition-root` | core | composition-root |
| `application-must-not-depend-on-adapters` | application | adapter |
| `application-must-not-depend-on-composition-root` | application | composition-root |
| `ports-must-not-depend-on-adapters` | port | adapter |
| `ports-must-not-depend-on-composition-root` | port | composition-root |
| `adapters-must-not-depend-on-composition-root` | adapter | composition-root |

The preset does not infer intent from folder names. The project supplies all
role patterns. The composition root is a distinct, narrow role rather than a
global bypass. The shipped example demonstrates a real, narrow exception from
an adapter startup hook to the composition root and names the exact preset rule
it exempts.
