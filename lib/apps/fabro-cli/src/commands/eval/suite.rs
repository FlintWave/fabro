use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, io};

use serde::Deserialize;
use toml::{self, de};

#[derive(Debug, thiserror::Error)]
pub(crate) enum SuiteError {
    #[error("could not read suite file {path}")]
    Read {
        path:   PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse suite file {path}")]
    Parse {
        path:   PathBuf,
        #[source]
        source: de::Error,
    },
    #[error("invalid suite: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuiteFile {
    #[serde(default)]
    _version: u64,
    suite:    RawSuiteMeta,
    #[serde(default, rename = "case")]
    cases:    Vec<RawCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuiteMeta {
    name:           String,
    #[serde(default = "default_threshold")]
    pass_threshold: f64,
}

fn default_threshold() -> f64 {
    1.0
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    name:        String,
    workflow:    PathBuf,
    #[serde(default)]
    target_from: Option<PathBuf>,
    #[serde(default)]
    goal_file:   Option<PathBuf>,
    #[serde(default)]
    inputs:      Option<BTreeMap<String, String>>,
    #[serde(default)]
    environment: Option<String>,
    #[serde(default)]
    timeout:     Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SuiteCase {
    pub(crate) name:        String,
    pub(crate) workflow:    PathBuf,
    pub(crate) target_from: PathBuf,
    pub(crate) goal_file:   Option<PathBuf>,
    pub(crate) inputs:      BTreeMap<String, String>,
    pub(crate) environment: Option<String>,
    pub(crate) timeout:     String,
}

#[derive(Debug, Clone)]
pub(crate) struct Suite {
    pub(crate) name:           String,
    pub(crate) pass_threshold: f64,
    pub(crate) cases:          Vec<SuiteCase>,
}

#[expect(clippy::disallowed_methods, reason = "sync suite loading")]
pub(crate) fn load_suite(path: &Path) -> Result<Suite, SuiteError> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let base_dir = canonical
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let content = fs::read_to_string(path).map_err(|source| SuiteError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_suite_str(&content, &base_dir).map_err(|err| match err {
        SuiteError::Parse { source, .. } => SuiteError::Parse {
            path: path.to_path_buf(),
            source,
        },
        other => other,
    })
}

pub(crate) fn parse_suite_str(content: &str, base_dir: &Path) -> Result<Suite, SuiteError> {
    let raw: RawSuiteFile = toml::from_str(content).map_err(|source| SuiteError::Parse {
        path: base_dir.to_path_buf(),
        source,
    })?;

    if !(0.0..=1.0).contains(&raw.suite.pass_threshold) {
        return Err(SuiteError::Validation(format!(
            "pass_threshold {} is out of range 0.0..=1.0",
            raw.suite.pass_threshold
        )));
    }

    let mut names = BTreeSet::new();
    if raw.cases.is_empty() {
        return Err(SuiteError::Validation(
            "suite must contain at least one case".into(),
        ));
    }
    for c in &raw.cases {
        if c.name.is_empty() {
            return Err(SuiteError::Validation("case name is required".into()));
        }
        if !c
            .name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        {
            return Err(SuiteError::Validation(format!(
                "invalid case name `{}`",
                c.name
            )));
        }
        if !names.insert(c.name.clone()) {
            return Err(SuiteError::Validation(format!(
                "duplicate case name `{}`",
                c.name
            )));
        }
    }

    let mut cases = Vec::new();
    for c in raw.cases {
        let workflow = resolve_checked(base_dir, &c.workflow, "workflow")?;
        let target_from = match c.target_from {
            Some(p) => resolve_checked(base_dir, &p, "target_from")?,
            None => base_dir.to_path_buf(),
        };
        // target_from should be a directory
        if !target_from.is_dir() {
            return Err(SuiteError::Validation(format!(
                "target_from path does not exist: {}",
                target_from.display()
            )));
        }
        let goal_file = match c.goal_file {
            Some(p) => Some(resolve_checked(base_dir, &p, "goal_file")?),
            None => None,
        };
        let timeout = c.timeout.unwrap_or_else(|| "60m".to_string());
        parse_duration(&timeout).map_err(SuiteError::Validation)?;
        let inputs = c.inputs.unwrap_or_default();
        cases.push(SuiteCase {
            name: c.name,
            workflow,
            target_from,
            goal_file,
            inputs,
            environment: c.environment,
            timeout,
        });
    }

    Ok(Suite {
        name: raw.suite.name,
        pass_threshold: raw.suite.pass_threshold,
        cases,
    })
}

fn resolve_checked(base_dir: &Path, p: &Path, label: &str) -> Result<PathBuf, SuiteError> {
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    };
    if !resolved.exists() {
        return Err(SuiteError::Validation(format!(
            "{label} path does not exist: {}",
            resolved.display()
        )));
    }
    Ok(resolved)
}

pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix('m') {
        if let Ok(n) = num.parse::<u64>() {
            return Ok(Duration::from_mins(n));
        }
    }
    if let Some(num) = s.strip_suffix('h') {
        if let Ok(n) = num.parse::<u64>() {
            return Ok(Duration::from_secs(n * 3600));
        }
    }
    if let Some(num) = s.strip_suffix('s') {
        if let Ok(n) = num.parse::<u64>() {
            return Ok(Duration::from_secs(n));
        }
    }
    if let Ok(n) = s.parse::<u64>() {
        return Ok(Duration::from_secs(n));
    }
    Err(format!("invalid timeout `{s}`"))
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "test infra")]
mod tests {
    use std::{env, fs};

    use fabro_util::error;
    use tempfile::tempdir;

    use super::*;

    fn write(p: &Path, contents: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    #[test]
    fn valid_suite_parses_with_defaults() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        write(&base.join("workflows/w.toml"), "x");
        let content = r#"
_version = 1
[suite]
name = "s"
[[case]]
name = "c"
workflow = "workflows/w.toml"
"#;
        let suite = parse_suite_str(content, base).unwrap();
        assert_eq!(suite.name, "s");
        assert!((suite.pass_threshold - 1.0).abs() < f64::EPSILON);
        assert_eq!(suite.cases.len(), 1);
        assert_eq!(suite.cases[0].timeout, "60m");
        assert_eq!(suite.cases[0].target_from, base.to_path_buf());
        assert!(suite.cases[0].goal_file.is_none());
        assert!(suite.cases[0].inputs.is_empty());
        let _ = parse_duration("30m").unwrap();
    }

    #[test]
    fn unknown_key_errors() {
        let dir = tempdir().unwrap();
        let content = r#"
[suite]
name = "s"
foo = 1
[[case]]
name = "c"
workflow = "w.toml"
"#;
        let err = parse_suite_str(content, dir.path()).unwrap_err();
        let chain = error::collect_chain(&err);
        assert!(
            chain.iter().any(|s| s.contains("unknown field `foo`")),
            "unexpected error: {chain:?}"
        );
    }

    #[test]
    fn empty_cases_errors() {
        let dir = tempdir().unwrap();
        let content = "[suite]\nname = \"s\"\n";
        assert!(parse_suite_str(content, dir.path()).is_err());
    }

    #[test]
    fn duplicate_name_errors() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        write(&base.join("w.toml"), "x");
        let content = r#"
[suite]
name = "s"
[[case]]
name = "c"
workflow = "w.toml"
[[case]]
name = "c"
workflow = "w.toml"
"#;
        assert!(parse_suite_str(content, base).is_err());
    }

    #[test]
    fn empty_name_errors() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        write(&base.join("w.toml"), "x");
        let content = r#"
[suite]
name = "s"
[[case]]
name = ""
workflow = "w.toml"
"#;
        assert!(parse_suite_str(content, base).is_err());
    }

    #[test]
    fn invalid_name_chars_errors() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        write(&base.join("w.toml"), "x");
        let content = r#"
[suite]
name = "s"
[[case]]
name = "C"
workflow = "w.toml"
"#;
        assert!(parse_suite_str(content, base).is_err());
    }

    #[test]
    fn threshold_out_of_range_errors() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        write(&base.join("w.toml"), "x");
        let content = r#"
[suite]
name = "s"
pass_threshold = 1.5
[[case]]
name = "c"
workflow = "w.toml"
"#;
        assert!(parse_suite_str(content, base).is_err());
    }

    #[test]
    fn missing_path_errors() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let content = r#"
[suite]
name = "s"
[[case]]
name = "c"
workflow = "nope.toml"
"#;
        assert!(parse_suite_str(content, base).is_err());
    }

    #[test]
    fn relative_paths_resolve_against_base() {
        let parent = tempdir().unwrap();
        let base = parent.path().join("suite_dir");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("w.toml"), "x").unwrap();
        fs::create_dir_all(base.join("repos")).unwrap();
        let cwd = tempdir().unwrap();
        let old = env::current_dir().unwrap();
        env::set_current_dir(cwd.path()).unwrap();
        let content = r#"
[suite]
name = "s"
[[case]]
name = "c"
workflow = "w.toml"
target_from = "repos"
"#;
        fs::create_dir_all(base.join("repos")).unwrap();
        let suite = parse_suite_str(content, &base).unwrap();
        assert_eq!(suite.cases[0].workflow, base.join("w.toml"));
        assert_eq!(suite.cases[0].target_from, base.join("repos"));
        env::set_current_dir(old).unwrap();
    }

    #[test]
    fn invalid_timeout_errors() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        write(&base.join("w.toml"), "x");
        let content = r#"
[suite]
name = "s"
[[case]]
name = "c"
workflow = "w.toml"
timeout = "30min"
"#;
        assert!(parse_suite_str(content, base).is_err());
    }

    #[test]
    fn load_suite_reads_file() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        write(&base.join("w.toml"), "x");
        let suite_file = base.join("suite.toml");
        write(
            &suite_file,
            r#"
[suite]
name = "s"
[[case]]
name = "c"
workflow = "w.toml"
"#,
        );
        let suite = load_suite(&suite_file).unwrap();
        assert_eq!(suite.name, "s");
    }
}
