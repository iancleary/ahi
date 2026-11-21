//! Version 2 groups common grants. Expand into the same validator and evaluator as v1.
use super::*;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Modes {
    #[serde(rename = "version")]
    _version: u32,
    default_mode: String,
    #[serde(default)]
    modes: BTreeMap<String, Mode>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Mode {
    allow: Permissions,
    ask: Permissions,
    deny: Permissions,
    rules: Vec<RawRule>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Permissions {
    tools: Vec<String>,
    files: Vec<Files>,
    workers: Vec<String>,
    local_profiles: Vec<String>,
    write_files: bool,
    skills: Vec<String>,
    skill_scripts: Vec<SkillScripts>,
    commands: Vec<Commands>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Files {
    tools: Vec<String>,
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SkillScripts {
    Scope(String),
    Group(ScriptGroup),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptGroup {
    skills: Vec<String>,
    scripts: Vec<String>,
    #[serde(default = "workspace_root")]
    working_dirs: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Commands {
    #[serde(default)]
    programs: Vec<String>,
    #[serde(default)]
    argv_prefixes: Vec<Vec<String>>,
    #[serde(default = "workspace_root")]
    working_dirs: Vec<String>,
    #[serde(default)]
    toolchain_env: Vec<String>,
}

fn workspace_root() -> Vec<String> {
    vec![".".into()]
}

impl Modes {
    pub(super) fn compile(self, origin: &str, builtin: &[&str]) -> Result<ActionPolicy> {
        let mut named = BTreeMap::new();
        for (name, mode) in self.modes {
            if !crate::precommit::policy::valid_rule_id(&name)
                || matches!(
                    name.as_str(),
                    "plan" | "trusted" | "full-access" | "read-only" | "read-write" | "policy"
                )
            {
                return Err(Error::new(format!(
                    "{origin}: invalid or reserved mode name `{name}`"
                )));
            }
            let mut rules = mode.rules;
            for (label, effect, ask, grants) in [
                ("allow", Effect::Approve, false, mode.allow),
                ("ask", Effect::Approve, true, mode.ask),
                ("deny", Effect::Deny, false, mode.deny),
            ] {
                let mut push = |action: &str, matcher: RawMatch, env| {
                    let id = format!("{label}-{}", rules.len() + 1);
                    rules.push(RawRule {
                        id,
                        action: action.into(),
                        matcher,
                        effect,
                        ask,
                        sandbox: None,
                        toolchain_env: env,
                    });
                };
                for tool in grants.tools {
                    push(
                        "tool_call",
                        RawMatch {
                            tool: Some(tool),
                            ..Default::default()
                        },
                        None,
                    );
                }
                for files in grants.files {
                    if files.tools.is_empty()
                        || files.paths.is_empty()
                        || files.paths.iter().any(|p| p.trim().is_empty())
                        || files.tools.len().saturating_mul(files.paths.len()) > 1024
                    {
                        return Err(Error::new(format!("{origin}: files requires nonempty tools and paths (at most 1024 pairs)")));
                    }
                    for tool in files.tools {
                        for path in &files.paths {
                            push(
                                if WRITE_TOOLS.contains(&tool.as_str()) {
                                    "file_write"
                                } else {
                                    "tool_call"
                                },
                                RawMatch {
                                    tool: Some(tool.clone()),
                                    path: Some(path.clone()),
                                    ..Default::default()
                                },
                                None,
                            );
                        }
                    }
                }
                for profile in grants.local_profiles {
                    push(
                        "delegate",
                        RawMatch {
                            local_profile: Some(profile),
                            ..Default::default()
                        },
                        None,
                    );
                }
                for worker in grants.workers {
                    push(
                        "delegate",
                        RawMatch {
                            worker: Some(worker),
                            ..Default::default()
                        },
                        None,
                    );
                }
                if grants.write_files {
                    push("file_write", RawMatch::default(), None);
                }
                for skill in grants.skills {
                    push(
                        "skill",
                        RawMatch {
                            skill: Some(skill),
                            ..Default::default()
                        },
                        None,
                    );
                }
                for entry in grants.skill_scripts {
                    let group = match entry {
                        SkillScripts::Group(group) => group,
                        SkillScripts::Scope(scope) => ScriptGroup {
                            skills: vec![scope],
                            scripts: vec!["scripts/**".into()],
                            working_dirs: workspace_root(),
                        },
                    };
                    let count = group
                        .skills
                        .len()
                        .saturating_mul(group.scripts.len())
                        .saturating_mul(group.working_dirs.len());
                    if !(1..=1024).contains(&count)
                        || group.working_dirs.iter().any(|p| p.trim().is_empty())
                    {
                        return Err(Error::new(format!("{origin}: skill script group requires 1–1024 skill/script/directory combinations")));
                    }
                    for skill in group.skills {
                        for script in &group.scripts {
                            for dir in &group.working_dirs {
                                push(
                                    "skill_command",
                                    RawMatch {
                                        skill: Some(skill.clone()),
                                        script: Some(script.clone()),
                                        path: Some(dir.clone()),
                                        ..Default::default()
                                    },
                                    None,
                                );
                            }
                        }
                    }
                }
                for commands in grants.commands {
                    let prefixes = match (
                        commands.programs.is_empty(),
                        commands.argv_prefixes.is_empty(),
                    ) {
                        (true, false) => commands.argv_prefixes,
                        (false, true) => commands.programs.into_iter().map(|p| vec![p]).collect(),
                        _ => {
                            return Err(Error::new(format!(
                                "{origin}: a command group needs either programs or argv_prefixes"
                            )))
                        }
                    };
                    if commands.working_dirs.is_empty()
                        || commands
                            .working_dirs
                            .iter()
                            .any(|dir| dir.trim().is_empty())
                    {
                        return Err(Error::new(format!(
                            "{origin}: working_dirs requires a nonempty list of directories"
                        )));
                    }
                    if prefixes.len().saturating_mul(commands.working_dirs.len()) > 1024 {
                        return Err(Error::new(format!(
                            "{origin}: command group exceeds 1024 prefix/directory pairs"
                        )));
                    }
                    for prefix in prefixes {
                        for dir in &commands.working_dirs {
                            push(
                                "command",
                                RawMatch {
                                    argv: Some(prefix.clone()),
                                    path: Some(dir.clone()),
                                    ..Default::default()
                                },
                                Some(commands.toolchain_env.clone()),
                            );
                        }
                    }
                }
            }
            let policy = compile_rules(
                RawPolicy {
                    version: 1,
                    default: None,
                    rules,
                },
                &format!("{origin}: mode {name}"),
                builtin,
            )?;
            named.insert(name, policy.rules);
        }
        let rules = if self.default_mode == "plan" {
            ActionPolicy::plan().rules
        } else {
            named.get(&self.default_mode).cloned().ok_or_else(|| {
                Error::new(format!(
                    "{origin}: unknown default_mode `{}`",
                    self.default_mode
                ))
            })?
        };
        Ok(ActionPolicy {
            rules,
            default_mode: Some(self.default_mode),
            named,
        })
    }
}
