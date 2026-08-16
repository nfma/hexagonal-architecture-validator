# Configuration

`hav` reads `hav.toml` from `--root` by default. `--config` accepts an absolute
path or a path relative to `--root`. Unknown fields and unsupported versions are
errors.

## Top-level schema

```toml
version = 1
preset = "hexagonal" # optional

[analysis]
strict = false       # optional, default false
detect_cycles = true # optional, default true
```

- `version` must be `1`.
- `preset` currently accepts `hexagonal`.
- `analysis.strict` makes item-position macros that cannot be expanded fatal.
  `include!` and unresolved source modules remain fatal in every mode.
- `analysis.detect_cycles` emits one deterministic `no-cycles` finding for
  each strongly connected component containing more than one module.

The CLI `--strict` flag enables strict analysis even when the config does not.

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
pattern assigns the role. Unclassified modules remain in the graph but do not
match role rules. Role regexes use unanchored matching unless the pattern
includes anchors such as `^` and `$`; anchor patterns when substring matches
would classify unintended modules or paths.

Module IDs are stable strings of the form:

```text
package::kind(target)::nested::module
```

For example, package `orders` with library target `orders` has root module
`orders::lib(orders)` and child `orders::lib(orders)::domain`. Paths use `/`
separators and are relative to the Cargo workspace root, including `..` when a
literal `#[path]` module is outside it.

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
id = "composition-root-wiring"
description = "Startup may assemble concrete adapters"
from = ["composition-root"]
to = ["adapter"]
```

For each dependency edge, `hav`:

1. classifies its source and target modules;
2. checks allowed rules first; any match explicitly exempts that edge;
3. otherwise emits one finding for each matching forbidden rule.

Allowed rules are exceptions, not a global allowlist. Give exception roles
narrow path or module patterns. Rule IDs are globally unique across forbidden,
allowed, and preset rules. When modules have overlapping roles, any allowed rule
matching the source and target role sets suppresses every forbidden rule for
that same edge.

In JSON, a `forbidden-dependency` finding includes `target` and `evidence` and
omits `cycle`. A `cycle` finding includes the sorted `cycle` members and omits
`target` and `evidence`. The common fields `rule_id`, `kind`, `message`, and
`source` are always present.

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
global bypass; use a named allowed rule to record its permitted wiring.
