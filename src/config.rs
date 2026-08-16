use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, bail};
use regex::Regex;
use serde::Deserialize;

const SUPPORTED_CONFIG_VERSION: u32 = 1;
const ID_PATTERN: &str = r"^[a-z][a-z0-9-]*$";
const HEXAGONAL_ROLE_IDS: [&str; 5] =
    ["core", "application", "port", "adapter", "composition-root"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u32,
    #[serde(default)]
    preset: Option<Preset>,
    #[serde(default)]
    analysis: AnalysisConfig,
    #[serde(default)]
    roles: Vec<RoleFile>,
    #[serde(default)]
    forbidden: Vec<RuleFile>,
    #[serde(default)]
    allowed: Vec<RuleFile>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Preset {
    Hexagonal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalysisConfig {
    pub strict: bool,
    pub detect_cycles: bool,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            strict: true,
            detect_cycles: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleFile {
    id: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    modules: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    id: String,
    #[serde(default)]
    description: Option<String>,
    from: Vec<String>,
    to: Vec<String>,
    exempts: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct Role {
    pub id: String,
    pub description: Option<String>,
    module_patterns: Vec<Regex>,
    path_patterns: Vec<Regex>,
}

impl Role {
    pub fn matches(&self, module: &str, path: &str) -> bool {
        self.module_patterns
            .iter()
            .any(|pattern| pattern.is_match(module))
            || self
                .path_patterns
                .iter()
                .any(|pattern| pattern.is_match(path))
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub description: Option<String>,
    pub from: BTreeSet<String>,
    pub to: BTreeSet<String>,
    pub exempts: BTreeSet<String>,
}

impl Rule {
    pub fn matches(&self, from: &BTreeSet<String>, to: &BTreeSet<String>) -> bool {
        !self.from.is_disjoint(from) && !self.to.is_disjoint(to)
    }
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub analysis: AnalysisConfig,
    pub roles: Vec<Role>,
    pub forbidden: Vec<Rule>,
    pub allowed: Vec<Rule>,
    pub preset_mandated_roles: BTreeSet<String>,
}

impl LoadedConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let file: ConfigFile = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in {}", path.display()))?;

        if file.version != SUPPORTED_CONFIG_VERSION {
            bail!(
                "unsupported config version {}; expected {}",
                file.version,
                SUPPORTED_CONFIG_VERSION
            );
        }
        if file.roles.is_empty() {
            bail!("config must declare at least one [[roles]] entry");
        }

        let id_pattern = Regex::new(ID_PATTERN).expect("static ID pattern is valid");
        let mut seen_role_ids = BTreeSet::new();
        let roles = file
            .roles
            .into_iter()
            .map(|role| compile_role(role, &id_pattern, &mut seen_role_ids))
            .collect::<anyhow::Result<Vec<_>>>()?;

        let preset_mandated_roles = match file.preset {
            Some(Preset::Hexagonal) => HEXAGONAL_ROLE_IDS.into_iter().map(str::to_owned).collect(),
            None => BTreeSet::new(),
        };
        let mut rules = file.forbidden;
        if !preset_mandated_roles.is_empty() {
            require_hexagonal_roles(&seen_role_ids)?;
            rules.extend(hexagonal_rules());
        }

        let mut seen_rule_ids = BTreeSet::new();
        let forbidden = rules
            .into_iter()
            .map(|rule| {
                compile_forbidden_rule(rule, &id_pattern, &seen_role_ids, &mut seen_rule_ids)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let forbidden_ids = forbidden
            .iter()
            .map(|rule| rule.id.clone())
            .collect::<BTreeSet<_>>();
        let allowed_ids = file
            .allowed
            .iter()
            .map(|rule| rule.id.clone())
            .collect::<BTreeSet<_>>();
        let allowed = file
            .allowed
            .into_iter()
            .map(|rule| {
                compile_allowed_rule(
                    rule,
                    &id_pattern,
                    &seen_role_ids,
                    &forbidden_ids,
                    &allowed_ids,
                    &mut seen_rule_ids,
                )
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(Self {
            analysis: file.analysis,
            roles,
            forbidden,
            allowed,
            preset_mandated_roles,
        })
    }
}

fn compile_role(
    role: RoleFile,
    id_pattern: &Regex,
    seen_ids: &mut BTreeSet<String>,
) -> anyhow::Result<Role> {
    validate_id("role", &role.id, id_pattern, seen_ids)?;
    if role.modules.is_empty() && role.paths.is_empty() {
        bail!(
            "role '{}' must declare at least one module or path pattern",
            role.id
        );
    }

    Ok(Role {
        id: role.id.clone(),
        description: role.description,
        module_patterns: compile_patterns("module", &role.id, role.modules)?,
        path_patterns: compile_patterns("path", &role.id, role.paths)?,
    })
}

fn compile_forbidden_rule(
    rule: RuleFile,
    id_pattern: &Regex,
    role_ids: &BTreeSet<String>,
    seen_ids: &mut BTreeSet<String>,
) -> anyhow::Result<Rule> {
    if rule.exempts.is_some() {
        bail!("forbidden rule '{}' cannot declare exempts", rule.id);
    }
    validate_id("rule", &rule.id, id_pattern, seen_ids)?;
    compile_rule(rule, role_ids, BTreeSet::new())
}

fn compile_allowed_rule(
    rule: RuleFile,
    id_pattern: &Regex,
    role_ids: &BTreeSet<String>,
    forbidden_ids: &BTreeSet<String>,
    allowed_ids: &BTreeSet<String>,
    seen_ids: &mut BTreeSet<String>,
) -> anyhow::Result<Rule> {
    validate_id("rule", &rule.id, id_pattern, seen_ids)?;
    let exempts = rule
        .exempts
        .as_ref()
        .filter(|ids| !ids.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("allowed rule '{}' must declare non-empty exempts", rule.id)
        })?
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for exempted in &exempts {
        if exempted == &rule.id {
            bail!(
                "allowed rule '{}' cannot exempt itself; expected a forbidden rule ID",
                rule.id
            );
        }
        if !forbidden_ids.contains(exempted) {
            if allowed_ids.contains(exempted) {
                bail!(
                    "allowed rule '{}' cannot exempt allowed rule '{}'",
                    rule.id,
                    exempted
                );
            }
            bail!(
                "allowed rule '{}' refers to unknown forbidden rule '{}'",
                rule.id,
                exempted
            );
        }
    }
    compile_rule(rule, role_ids, exempts)
}

fn compile_rule(
    rule: RuleFile,
    role_ids: &BTreeSet<String>,
    exempts: BTreeSet<String>,
) -> anyhow::Result<Rule> {
    if rule.from.is_empty() || rule.to.is_empty() {
        bail!(
            "rule '{}' must declare non-empty from and to lists",
            rule.id
        );
    }

    let from = rule.from.into_iter().collect::<BTreeSet<_>>();
    let to = rule.to.into_iter().collect::<BTreeSet<_>>();
    for role in from.iter().chain(to.iter()) {
        if !role_ids.contains(role) {
            bail!("rule '{}' refers to unknown role '{}'", rule.id, role);
        }
    }

    Ok(Rule {
        id: rule.id,
        description: rule.description,
        from,
        to,
        exempts,
    })
}

fn validate_id(
    kind: &str,
    id: &str,
    pattern: &Regex,
    seen_ids: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    if !pattern.is_match(id) {
        bail!("{kind} ID '{id}' must match {ID_PATTERN}");
    }
    if !seen_ids.insert(id.to_owned()) {
        bail!("duplicate {kind} ID '{id}'");
    }
    Ok(())
}

fn compile_patterns(kind: &str, role: &str, patterns: Vec<String>) -> anyhow::Result<Vec<Regex>> {
    patterns
        .into_iter()
        .map(|pattern| {
            Regex::new(&pattern)
                .with_context(|| format!("invalid {kind} pattern '{pattern}' in role '{role}'"))
        })
        .collect()
}

fn require_hexagonal_roles(role_ids: &BTreeSet<String>) -> anyhow::Result<()> {
    for required in HEXAGONAL_ROLE_IDS {
        if !role_ids.contains(required) {
            bail!("hexagonal preset requires a '{required}' role");
        }
    }
    Ok(())
}

fn hexagonal_rules() -> Vec<RuleFile> {
    vec![
        RuleFile {
            id: "core-must-not-depend-on-adapters".to_owned(),
            description: Some("Core code must not depend on concrete adapters".to_owned()),
            from: vec!["core".to_owned()],
            to: vec!["adapter".to_owned()],
            exempts: None,
        },
        RuleFile {
            id: "core-must-not-depend-on-composition-root".to_owned(),
            description: Some("Core code must not depend on application wiring".to_owned()),
            from: vec!["core".to_owned()],
            to: vec!["composition-root".to_owned()],
            exempts: None,
        },
        RuleFile {
            id: "application-must-not-depend-on-adapters".to_owned(),
            description: Some("Application code must depend on ports, not adapters".to_owned()),
            from: vec!["application".to_owned()],
            to: vec!["adapter".to_owned()],
            exempts: None,
        },
        RuleFile {
            id: "application-must-not-depend-on-composition-root".to_owned(),
            description: Some("Application code must not depend on application wiring".to_owned()),
            from: vec!["application".to_owned()],
            to: vec!["composition-root".to_owned()],
            exempts: None,
        },
        RuleFile {
            id: "ports-must-not-depend-on-adapters".to_owned(),
            description: Some("Ports must not depend on concrete adapters".to_owned()),
            from: vec!["port".to_owned()],
            to: vec!["adapter".to_owned()],
            exempts: None,
        },
        RuleFile {
            id: "ports-must-not-depend-on-composition-root".to_owned(),
            description: Some("Ports must not depend on application wiring".to_owned()),
            from: vec!["port".to_owned()],
            to: vec!["composition-root".to_owned()],
            exempts: None,
        },
        RuleFile {
            id: "adapters-must-not-depend-on-composition-root".to_owned(),
            description: Some("Adapters must not depend on application wiring".to_owned()),
            from: vec!["adapter".to_owned()],
            to: vec!["composition-root".to_owned()],
            exempts: None,
        },
    ]
}

pub fn classify_modules(
    config: &LoadedConfig,
    modules: impl Iterator<Item = (String, String, String)>,
) -> BTreeMap<String, BTreeSet<String>> {
    modules
        .map(|(id, module, path)| {
            let roles = config
                .roles
                .iter()
                .filter(|role| role.matches(&module, &path))
                .map(|role| role.id.clone())
                .collect();
            (id, roles)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_config_fields() {
        let error = toml::from_str::<ConfigFile>("version = 1\nunknown = true\n")
            .expect_err("unknown fields should fail");
        assert!(error.to_string().contains("unknown field"));
    }
}
