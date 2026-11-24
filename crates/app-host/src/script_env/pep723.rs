use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Default managed Python minor version used when a script leaves `python` unspecified.
pub const DEFAULT_SCRIPT_PYTHON_VERSION: &str = "3.12";

/// Failures raised while parsing PEP 723 metadata or preparing script environments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptEnvError {
    InvalidPep723 { reason: String },
    DependencyConflict { reason: String },
    InvalidSpec { reason: String },
    UvUnavailable { reason: String },
    PythonUnavailable { python: String, reason: String },
    LockContended { env_hash: String },
    PrepareFailed { reason: String },
    Io { path: PathBuf, message: String },
}

impl fmt::Display for ScriptEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPep723 { reason } => write!(f, "invalid PEP 723 metadata: {reason}"),
            Self::DependencyConflict { reason } => {
                write!(f, "script dependency conflict: {reason}")
            }
            Self::InvalidSpec { reason } => write!(f, "invalid script environment spec: {reason}"),
            Self::UvUnavailable { reason } => write!(f, "`uv` runtime unavailable: {reason}"),
            Self::PythonUnavailable { python, reason } => {
                write!(f, "managed Python `{python}` unavailable: {reason}")
            }
            Self::LockContended { env_hash } => {
                write!(
                    f,
                    "script environment `{env_hash}` is locked by another operation"
                )
            }
            Self::PrepareFailed { reason } => {
                write!(f, "failed to prepare script environment: {reason}")
            }
            Self::Io { path, message } => {
                write!(f, "I/O error at `{}`: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for ScriptEnvError {}

/// Parsed PEP 723 `# /// script` inline metadata header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pep723Metadata {
    pub requires_python: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
struct Pep723Toml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requires_python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dependencies: Option<Vec<String>>,
}

fn is_valid_block_type(kind: &str) -> bool {
    !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Extract and parse the PEP 723 `# /// script` block from Python source `code`.
///
/// Enforces PEP 723 uniqueness: multiple `# /// script` blocks or malformed /
/// unclosed blocks return [`ScriptEnvError::InvalidPep723`].
pub fn parse_pep723_metadata(code: &str) -> Result<Option<Pep723Metadata>, ScriptEnvError> {
    let mut lines = code.lines().enumerate().peekable();
    let mut found_script_body: Option<String> = None;

    while let Some((line_idx, raw_line)) = lines.next() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let Some(block_type) = line.strip_prefix("# /// ") else {
            continue;
        };
        if !is_valid_block_type(block_type) {
            continue;
        }

        let mut body_lines = Vec::new();
        let mut closed = false;
        for (_, next_raw) in lines.by_ref() {
            let next = next_raw.strip_suffix('\r').unwrap_or(next_raw);
            if next == "# ///" {
                closed = true;
                break;
            }
            if let Some(rest) = next.strip_prefix("# ") {
                body_lines.push(rest);
            } else if next == "#" {
                body_lines.push("");
            } else if block_type == "script" {
                return Err(ScriptEnvError::InvalidPep723 {
                    reason: format!(
                        "line {} inside `# /// script` block must begin with `# ` or `#`",
                        line_idx + 1
                    ),
                });
            } else {
                break;
            }
        }

        if block_type != "script" {
            continue;
        }
        if !closed {
            return Err(ScriptEnvError::InvalidPep723 {
                reason: "unclosed `# /// script` metadata block (missing closing `# ///`)"
                    .to_owned(),
            });
        }
        if found_script_body.is_some() {
            return Err(ScriptEnvError::InvalidPep723 {
                reason: "multiple `# /// script` metadata blocks are not allowed by PEP 723"
                    .to_owned(),
            });
        }
        found_script_body = Some(body_lines.join("\n"));
    }

    let Some(body) = found_script_body else {
        return Ok(None);
    };

    let parsed: Pep723Toml = toml::from_str(&body).map_err(|e| ScriptEnvError::InvalidPep723 {
        reason: format!("invalid TOML inside `# /// script`: {e}"),
    })?;

    let requires_python = match parsed.requires_python {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.contains('\0') {
                return Err(ScriptEnvError::InvalidPep723 {
                    reason: "`requires-python` must be a non-empty version specifier".to_owned(),
                });
            }
            Some(trimmed.to_owned())
        }
        None => None,
    };

    let mut dependencies = Vec::new();
    if let Some(raw_deps) = parsed.dependencies {
        for dep in raw_deps {
            let trimmed = dep.trim();
            if trimmed.is_empty() || trimmed.contains('\0') || trimmed.contains('\n') {
                return Err(ScriptEnvError::InvalidPep723 {
                    reason: "PEP 723 `dependencies` entries must be non-empty single-line strings"
                        .to_owned(),
                });
            }
            dependencies.push(trimmed.to_owned());
        }
    }

    Ok(Some(Pep723Metadata {
        requires_python,
        dependencies,
    }))
}

/// Format an empty-body Python stub carrying only PEP 723 metadata for `uv lock --script`.
#[must_use]
pub fn format_pep723_stub(requires_python: Option<&str>, dependencies: &[String]) -> String {
    let raw = Pep723Toml {
        requires_python: requires_python.map(str::to_owned),
        dependencies: Some(dependencies.to_vec()),
    };
    let toml_text = toml::to_string(&raw).unwrap_or_default();
    let mut out = String::from("# /// script\n");
    for line in toml_text.lines() {
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("# ///\n");
    out
}

/// Canonical target triple for the active host OS and CPU architecture.
#[must_use]
pub fn host_target_triple() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin".to_owned(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_owned(),
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu".to_owned(),
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu".to_owned(),
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_owned(),
        ("windows", "aarch64") => "aarch64-pc-windows-msvc".to_owned(),
        (os, arch) => format!("{arch}-{os}"),
    }
}

/// Normalized, content-addressed environment specification for a Python script step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEnvSpec {
    pub python: String,
    pub requires_python: Option<String>,
    pub dependencies: Vec<String>,
    pub allow_source_build: bool,
    pub target_triple: String,
}

impl ScriptEnvSpec {
    /// Merge inline PEP 723 metadata with explicit `python` / `dependencies` inputs,
    /// validating conflicts and normalizing dependency ordering.
    pub fn resolve(
        code: &str,
        explicit_python: Option<&str>,
        explicit_deps: &[String],
        allow_source_build: bool,
    ) -> Result<Self, ScriptEnvError> {
        let inline = parse_pep723_metadata(code)?;
        let inline_requires = inline.as_ref().and_then(|m| m.requires_python.as_deref());
        let inline_deps = inline
            .as_ref()
            .map(|m| m.dependencies.as_slice())
            .unwrap_or(&[]);

        let (python, requires_python) = resolve_python_spec(inline_requires, explicit_python)?;
        let dependencies = merge_dependencies(inline_deps, explicit_deps)?;

        Ok(Self {
            python,
            requires_python,
            dependencies,
            allow_source_build,
            target_triple: host_target_triple(),
        })
    }

    /// Deterministic SHA-256 cache key over `(python, target_triple, dependencies, allow_source_build)`.
    #[must_use]
    pub fn env_hash(&self) -> String {
        #[derive(Serialize)]
        struct CanonicalKey<'a> {
            schema_version: u16,
            python: &'a str,
            target_triple: &'a str,
            dependencies: &'a [String],
            allow_source_build: bool,
        }

        let payload = CanonicalKey {
            schema_version: 1,
            python: &self.python,
            target_triple: &self.target_triple,
            dependencies: &self.dependencies,
            allow_source_build: self.allow_source_build,
        };
        let bytes =
            serde_json::to_vec(&payload).expect("canonical key serialization is infallible");
        hex::encode(Sha256::digest(bytes))
    }
}

fn validate_python_token(raw: &str) -> Result<String, ScriptEnvError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains('\0')
        || trimmed.bytes().any(|b| b.is_ascii_whitespace())
    {
        return Err(ScriptEnvError::InvalidSpec {
            reason: format!("invalid python version request `{raw}`"),
        });
    }
    Ok(trimmed.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PyVer {
    major: u32,
    minor: u32,
    patch: u32,
}

fn parse_simple_py_ver(raw: &str) -> Option<(PyVer, usize)> {
    let trimmed = raw.trim().strip_suffix(".*").unwrap_or(raw.trim());
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = if parts.len() >= 2 {
        parts[1].parse::<u32>().ok()?
    } else {
        0
    };
    let patch = if parts.len() == 3 {
        parts[2].parse::<u32>().ok()?
    } else {
        0
    };
    Some((
        PyVer {
            major,
            minor,
            patch,
        },
        parts.len(),
    ))
}

fn specifier_satisfies(spec: &str, ver: PyVer, wildcard_prefix: bool) -> Result<bool, ()> {
    for raw_clause in spec.split(',') {
        let clause = raw_clause.trim();
        if clause.is_empty() {
            continue;
        }
        let ok = if let Some(rest) = clause.strip_prefix(">=") {
            let (bound, _) = parse_simple_py_ver(rest).ok_or(())?;
            ver >= bound
        } else if let Some(rest) = clause.strip_prefix("<=") {
            let (bound, parts) = parse_simple_py_ver(rest).ok_or(())?;
            if parts == 2 && wildcard_prefix {
                (ver.major, ver.minor) <= (bound.major, bound.minor)
            } else {
                ver <= bound
            }
        } else if let Some(rest) = clause.strip_prefix("==") {
            let clean = rest.trim();
            if let Some(prefix) = clean.strip_suffix(".*") {
                let (bound, parts) = parse_simple_py_ver(prefix).ok_or(())?;
                if parts == 1 {
                    ver.major == bound.major
                } else {
                    (ver.major, ver.minor) == (bound.major, bound.minor)
                }
            } else {
                let (bound, parts) = parse_simple_py_ver(clean).ok_or(())?;
                if parts == 2 && wildcard_prefix {
                    (ver.major, ver.minor) == (bound.major, bound.minor)
                } else {
                    ver == bound
                }
            }
        } else if let Some(rest) = clause.strip_prefix("!=") {
            let clean = rest.trim();
            if let Some(prefix) = clean.strip_suffix(".*") {
                let (bound, parts) = parse_simple_py_ver(prefix).ok_or(())?;
                if parts == 1 {
                    ver.major != bound.major
                } else {
                    (ver.major, ver.minor) != (bound.major, bound.minor)
                }
            } else {
                let (bound, _) = parse_simple_py_ver(clean).ok_or(())?;
                ver != bound
            }
        } else if let Some(rest) = clause.strip_prefix("~=") {
            let (bound, parts) = parse_simple_py_ver(rest).ok_or(())?;
            if parts >= 3 {
                ver.major == bound.major && ver.minor == bound.minor && ver >= bound
            } else {
                ver.major == bound.major && ver >= bound
            }
        } else if let Some(rest) = clause.strip_prefix('>') {
            let (bound, _) = parse_simple_py_ver(rest).ok_or(())?;
            ver > bound
        } else if let Some(rest) = clause.strip_prefix('<') {
            let (bound, _) = parse_simple_py_ver(rest).ok_or(())?;
            ver < bound
        } else {
            return Err(());
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

fn pick_python_for_requires(spec: &str) -> Option<String> {
    // Prefer the workspace default minor version when compatible; otherwise scan supported 3.x minors.
    let (default_ver, _) = parse_simple_py_ver(DEFAULT_SCRIPT_PYTHON_VERSION)?;
    if specifier_satisfies(spec, default_ver, true) == Ok(true) {
        return Some(DEFAULT_SCRIPT_PYTHON_VERSION.to_owned());
    }
    for minor in (9..=15).rev() {
        let candidate = PyVer {
            major: 3,
            minor,
            patch: 0,
        };
        if specifier_satisfies(spec, candidate, true) == Ok(true) {
            return Some(format!("3.{minor}"));
        }
    }
    None
}

fn resolve_python_spec(
    inline_requires: Option<&str>,
    explicit_python: Option<&str>,
) -> Result<(String, Option<String>), ScriptEnvError> {
    match (inline_requires, explicit_python) {
        (None, None) => Ok((
            DEFAULT_SCRIPT_PYTHON_VERSION.to_owned(),
            Some(format!("=={DEFAULT_SCRIPT_PYTHON_VERSION}.*")),
        )),
        (None, Some(explicit)) => {
            let py = validate_python_token(explicit)?;
            let req = if parse_simple_py_ver(&py).is_some() {
                Some(format!("=={py}.*"))
            } else {
                None
            };
            Ok((py, req))
        }
        (Some(inline), None) => {
            let python = pick_python_for_requires(inline)
                .unwrap_or_else(|| DEFAULT_SCRIPT_PYTHON_VERSION.to_owned());
            Ok((python, Some(inline.to_owned())))
        }
        (Some(inline), Some(explicit)) => {
            let py = validate_python_token(explicit)?;
            if py == inline {
                return Ok((py, Some(inline.to_owned())));
            }
            if let Some((ver, parts)) = parse_simple_py_ver(&py) {
                match specifier_satisfies(inline, ver, parts <= 2) {
                    Ok(true) => return Ok((py, Some(inline.to_owned()))),
                    Ok(false) => {
                        return Err(ScriptEnvError::DependencyConflict {
                            reason: format!(
                                "explicit python `{py}` conflicts with PEP 723 `requires-python = \"{inline}\"`"
                            ),
                        });
                    }
                    Err(()) => {}
                }
            }
            Err(ScriptEnvError::DependencyConflict {
                reason: format!(
                    "explicit python `{py}` conflicts with PEP 723 `requires-python = \"{inline}\"`"
                ),
            })
        }
    }
}

fn canonicalize_pep503_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        }
    }
    out
}

#[derive(Debug)]
struct NormalizedDep {
    key: String,
    normalized_spec: String,
    canonical_entry: String,
}

fn parse_dependency_entry(raw: &str) -> Result<NormalizedDep, ScriptEnvError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.contains('\n') {
        return Err(ScriptEnvError::InvalidSpec {
            reason: "dependency requirement must be a non-empty single-line string".to_owned(),
        });
    }

    let name_end = trimmed
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(trimmed.len());
    let raw_name = &trimmed[..name_end];
    if raw_name.is_empty() {
        return Err(ScriptEnvError::InvalidSpec {
            reason: format!("missing package name in dependency `{trimmed}`"),
        });
    }
    let canonical_name = canonicalize_pep503_name(raw_name);
    let rest = trimmed[name_end..].trim();
    let compact_rest: String = rest.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let canonical_entry = if rest.is_empty() {
        canonical_name.clone()
    } else {
        format!("{canonical_name}{rest}")
    };

    Ok(NormalizedDep {
        key: canonical_name,
        normalized_spec: compact_rest,
        canonical_entry,
    })
}

fn merge_dependencies(
    inline_deps: &[String],
    explicit_deps: &[String],
) -> Result<Vec<String>, ScriptEnvError> {
    let mut by_pkg: BTreeMap<String, (String, String)> = BTreeMap::new();
    for raw in inline_deps.iter().chain(explicit_deps.iter()) {
        let parsed = parse_dependency_entry(raw)?;
        if let Some((existing_spec, existing_raw)) = by_pkg.get(&parsed.key) {
            if existing_spec != &parsed.normalized_spec {
                return Err(ScriptEnvError::DependencyConflict {
                    reason: format!(
                        "conflicting requirements for `{}`: `{existing_raw}` vs `{}`",
                        parsed.key,
                        raw.trim()
                    ),
                });
            }
        } else {
            by_pkg.insert(parsed.key, (parsed.normalized_spec, parsed.canonical_entry));
        }
    }
    Ok(by_pkg.into_values().map(|(_, entry)| entry).collect())
}
