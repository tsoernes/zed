use crate::schema::json_schema_for;
use action_log::ActionLog;
use anyhow::{Result, anyhow};
use assistant_tool::{Tool, ToolResult};
use assistant_tool::{ToolResultContent, ToolResultOutput};
use gpui::{AnyWindowHandle, App, AppContext, Entity, Task};
use language_model::{LanguageModel, LanguageModelRequest, LanguageModelToolSchemaFormat};
use portable_pty as _; // keep dependency aligned with other tools (not used directly here)
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

/// Batches are processed with bounded concurrency.
/// This avoids spawning an unbounded number of threads in large PATH environments.
const DEFAULT_MAX_CONCURRENCY: usize = 12;
const DEFAULT_VERSION_TIMEOUT_MS: u64 = 1500;

/// Category -> candidate binaries.
/// Order is stable for deterministic output.
const CANDIDATE_GROUPS: &[(&str, &[&str])] = &[
    (
        "package_managers",
        &[
            "pnpm", "yarn", "npm", "pip", "pipx", "uv", "uvx", "poetry", "cargo", "rustup",
        ],
    ),
    (
        "rust_tools",
        &["cargo", "rustc", "rustfmt", "clippy-driver", "wasm-pack"],
    ),
    (
        "python_quality",
        &[
            "black",
            "ruff",
            "mypy",
            "isort",
            "pytest",
            "basedpyright",
            "pyright",
        ],
    ),
    (
        "search_productivity",
        &["rg", "ag", "fd", "fzf", "jq", "exa", "bat", "tree", "tldr"],
    ),
    ("system_perf", &["htop", "lsof", "strace", "perf", "time"]),
    (
        "containers",
        &[
            "docker", "podman", "nerdctl", "kubectl", "helm", "kind", "minikube", "k9s",
        ],
    ),
    (
        "cloud",
        &["aws", "az", "gcloud", "doctl", "flyctl", "heroku"],
    ),
    ("infra", &["terraform", "ansible", "pulumi", "packer"]),
    (
        "networking",
        &[
            "curl",
            "wget",
            "dig",
            "host",
            "nslookup",
            "ip",
            "traceroute",
        ],
    ),
    (
        "databases",
        &["sqlite3", "psql", "mysql", "redis-cli", "mongosh", "duckdb"],
    ),
    ("docs", &["pandoc", "sphinx-build", "mkdocs", "mdbook"]),
    ("vcs", &["git", "gh"]),
];

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DetectBinariesToolInput {
    /// Optional list limiting which categories to scan. Unknown categories are ignored.
    #[serde(default)]
    filter_categories: Option<Vec<String>>,
    /// Soft upper bound on concurrent version probes (minimum = 1).
    #[serde(default = "default_max_concurrency")]
    max_concurrency: usize,
    /// Timeout (ms) for each version probe attempt.
    #[serde(default = "default_version_timeout_ms")]
    version_timeout_ms: u64,
    /// If true, include raw errors for failed version probes (spawn/timeout); otherwise omit.
    #[serde(default)]
    include_errors: bool,
}

fn default_max_concurrency() -> usize {
    DEFAULT_MAX_CONCURRENCY
}

fn default_version_timeout_ms() -> u64 {
    DEFAULT_VERSION_TIMEOUT_MS
}

#[derive(Debug, Serialize)]
struct BinaryReport {
    name: String,
    category: String,
    found: bool,
    path: Option<String>,
    version: Option<String>,
    elapsed_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct Summary {
    total_scanned: usize,
    found: usize,
    missing: usize,
    categories_scanned: Vec<String>,
    version_timeout_ms: u64,
}

#[derive(Debug, Serialize)]
struct DetectBinariesResult {
    binaries: Vec<BinaryReport>,
    summary: Summary,
}

pub struct DetectBinariesTool;

impl DetectBinariesTool {
    pub const NAME: &str = "detect_binaries";
}

impl Tool for DetectBinariesTool {
    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    fn needs_confirmation(
        &self,
        _input: &serde_json::Value,
        _project: &Entity<Project>,
        _cx: &App,
    ) -> bool {
        true
    }

    fn may_perform_edits(&self) -> bool {
        false
    }

    fn description(&self) -> String {
        "Enumerate common developer binaries (by category), discover their install paths, and attempt to capture a short version string. Supports category filtering and bounded concurrency.".into()
    }

    fn icon(&self) -> ui::IconName {
        ui::IconName::ToolSearch
    }

    fn input_schema(
        &self,
        format: LanguageModelToolSchemaFormat,
    ) -> anyhow::Result<serde_json::Value> {
        json_schema_for::<DetectBinariesToolInput>(format)
    }

    fn ui_text(&self, input: &serde_json::Value) -> String {
        match serde_json::from_value::<DetectBinariesToolInput>(input.clone()) {
            Ok(parsed) => {
                if let Some(cats) = parsed.filter_categories.as_ref() {
                    format!(
                        "Detect binaries in: {}",
                        cats.iter()
                            .map(|c| c.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    "Detect common developer binaries".into()
                }
            }
            Err(_) => "Detect common developer binaries".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: serde_json::Value,
        _request: Arc<LanguageModelRequest>,
        _project: Entity<Project>,
        _action_log: Entity<ActionLog>,
        _model: Arc<dyn LanguageModel>,
        _window: Option<AnyWindowHandle>,
        cx: &mut App,
    ) -> ToolResult {
        let input: DetectBinariesToolInput = match serde_json::from_value(input) {
            Ok(i) => i,
            Err(e) => return Task::ready(Err(anyhow!(e))).into(),
        };

        let task: Task<anyhow::Result<ToolResultOutput>> = cx.background_spawn(async move {
            let filter_set: Option<HashSet<String>> = input
                .filter_categories
                .as_ref()
                .map(|v| v.iter().map(|s| s.to_lowercase()).collect());

            let mut targets: Vec<(String, String)> = Vec::new();
            let mut used_categories = vec![];
            for (cat, names) in CANDIDATE_GROUPS {
                let cat_lower = cat.to_string();
                if let Some(fs) = &filter_set {
                    if !fs.contains(&cat_lower) {
                        continue;
                    }
                }
                used_categories.push(cat_lower.clone());
                for b in *names {
                    targets.push((cat_lower.clone(), (*b).to_string()));
                }
            }

            let max_conc = input.max_concurrency.max(1);
            let timeout = input.version_timeout_ms;
            let include_errors = input.include_errors;
            let reports = Arc::new(Mutex::new(Vec::<BinaryReport>::new()));

            for chunk in targets.chunks(max_conc) {
                let mut handles = Vec::with_capacity(chunk.len());
                for (category, name) in chunk.iter().cloned() {
                    let reports = Arc::clone(&reports);
                    handles.push(thread::spawn(move || {
                        let start = Instant::now();
                        let paths = which_all(&name);
                        if paths.is_empty() {
                            reports.lock().unwrap().push(BinaryReport {
                                name,
                                category,
                                found: false,
                                path: None,
                                version: None,
                                elapsed_ms: Some(start.elapsed().as_millis()),
                                error: None,
                            });
                            return;
                        }
                        // Only disclose concrete paths if multiple occurrences are found (privacy / noise reduction).
                        let path_field = if paths.len() > 1 {
                            Some(paths.join(";"))
                        } else {
                            None
                        };
                        let probe_path = &paths[0];
                        let version_res = detect_version_with_timeout(probe_path, timeout);
                        let elapsed_ms = start.elapsed().as_millis();

                        let (version, error) = match version_res {
                            Ok(v) => (Some(v), None),
                            Err(e) => {
                                if include_errors {
                                    (None, Some(e.to_string()))
                                } else {
                                    (None, None)
                                }
                            }
                        };

                        reports.lock().unwrap().push(BinaryReport {
                            name,
                            category,
                            found: true,
                            path: path_field.clone(),
                            version,
                            elapsed_ms: Some(elapsed_ms),
                            error,
                        });
                    }));
                }
                for h in handles {
                    let _ = h.join();
                }
            }

            let mut collected = Arc::try_unwrap(reports).unwrap().into_inner().unwrap();
            collected.sort_by(|a, b| {
                (a.category.as_str(), a.name.as_str()).cmp(&(b.category.as_str(), b.name.as_str()))
            });

            let found_count = collected.iter().filter(|r| r.found).count();
            let result = DetectBinariesResult {
                binaries: collected,
                summary: Summary {
                    total_scanned: targets.len(),
                    found: found_count,
                    missing: targets.len().saturating_sub(found_count),
                    categories_scanned: used_categories,
                    version_timeout_ms: timeout,
                },
            };

            let json = serde_json::to_string_pretty(&result)?;
            let value = serde_json::to_value(&result)?;
            Ok(ToolResultOutput {
                content: ToolResultContent::Text(json),
                output: Some(value),
            })
        });

        ToolResult {
            output: task,
            card: None,
        }
    }
}

/// Locate all occurrences of a binary in PATH. Returns every executable match in PATH order.
/// The caller decides how (or whether) to disclose paths; we only surface them when >1 match.
fn which_all(name: &str) -> Vec<String> {
    let mut matches = Vec::new();
    let path_val = match env::var_os("PATH") {
        Some(p) => p,
        None => return matches,
    };
    for dir in env::split_paths(&path_val) {
        let candidate = dir.join(name);
        if candidate.is_file() && is_executable(&candidate) {
            if let Some(s) = candidate.to_str() {
                matches.push(s.to_string());
            }
        }
    }
    matches
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Try common version flags with a coarse timeout. Threads are not forcibly killed if they exceed
/// the timeout; we treat missing completion as timeout and move on. This avoids extra async deps.
fn detect_version_with_timeout(path: &str, timeout_ms: u64) -> Result<String> {
    let attempts: &[&[&str]] = &[&["--version"], &["version"], &["-V"]];
    let shared = Arc::new(Mutex::new(None::<Result<String>>));

    for flags in attempts {
        let shared_clone = Arc::clone(&shared);
        let path_string = path.to_string();
        let flags_vec: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
        let start = Instant::now();

        let _handle = thread::spawn(move || {
            let out = Command::new(&path_string).args(&flags_vec).output();
            let res = match out {
                Ok(o) => {
                    if o.stdout.is_empty() && o.stderr.is_empty() {
                        Err(anyhow!("no output"))
                    } else {
                        let text = if !o.stdout.is_empty() {
                            String::from_utf8_lossy(&o.stdout).into_owned()
                        } else {
                            String::from_utf8_lossy(&o.stderr).into_owned()
                        };
                        let first = text.lines().next().unwrap_or("").trim();
                        if first.is_empty() {
                            Err(anyhow!("empty first line"))
                        } else {
                            Ok(first.to_string())
                        }
                    }
                }
                Err(e) => Err(anyhow!("spawn failed: {e}")),
            };
            let mut guard = shared_clone.lock().unwrap();
            if guard.is_none() {
                *guard = Some(res);
            }
        });

        let timeout = Duration::from_millis(timeout_ms);
        while start.elapsed() < timeout {
            // Check if thread wrote result
            if shared.lock().unwrap().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        // If we have a result, return it.
        if let Some(res) = {
            let mut guard = shared.lock().unwrap();
            guard.take()
        } {
            return res;
        }

        // Timed out on this attempt; continue to next variant. Thread may finish later; result ignored.
    }

    Err(anyhow!("version probe timeout after {timeout_ms}ms"))
}
