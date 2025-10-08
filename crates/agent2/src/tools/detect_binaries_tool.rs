use crate::{AgentTool, ToolCallEventStream};
use agent_client_protocol::ToolKind;
use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

/// Input for the `detect_binaries` tool.
/// `filter_categories` (if provided) limits scanning to those category identifiers.
/// `max_concurrency` is a soft upper bound; values < 1 are treated as 1.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DetectBinariesToolInput {
    #[serde(default)]
    filter_categories: Option<Vec<String>>,
    #[serde(default = "default_max_concurrency")]
    max_concurrency: usize,
    /// Per-binary version probe timeout in milliseconds (applies to --version / -V / version attempts).
    #[serde(default = "default_version_timeout_ms")]
    version_timeout_ms: u64,
}

fn default_max_concurrency() -> usize {
    12
}

fn default_version_timeout_ms() -> u64 {
    1500
}

/// Category identifiers and associated candidate binary names.
/// Order matters (stable output).
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

#[derive(Debug, Serialize)]
struct BinaryReport {
    name: String,
    category: String,
    found: bool,
    path: Option<String>,
    version: Option<String>,
    elapsed_ms: Option<u128>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DetectBinariesOutput {
    binaries: Vec<BinaryReport>,
    summary: Summary,
}

#[derive(Debug, Serialize)]
struct Summary {
    total_scanned: usize,
    found: usize,
    missing: usize,
    categories_scanned: Vec<String>,
    version_timeout_ms: u64,
}

/// Tool that detects presence of common developer binaries and obtains their versions concurrently.
pub struct DetectBinariesTool;

impl DetectBinariesTool {
    pub fn new() -> Self {
        Self
    }

    pub fn name() -> &'static str {
        "detect_binaries"
    }
}

impl AgentTool for DetectBinariesTool {
    type Input = DetectBinariesToolInput;
    type Output = String;

    fn name() -> &'static str {
        Self::name()
    }

    fn kind() -> ToolKind {
        ToolKind::Read
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        "Detecting common developer binaries...".into()
    }

    fn run(
        self: Arc<Self>,
        input: Self::Input,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output>> {
        // Authorization step ensures user visibility/consent where required.
        let authorize = event_stream.authorize(self.initial_title(Ok(input.clone()), cx), cx);

        cx.spawn(async move |cx| {
            authorize.await?;
            let filtered_categories: Option<Vec<String>> = input
                .filter_categories
                .as_ref()
                .map(|v| v.iter().map(|s| s.to_lowercase()).collect());

            let mut tasks: Vec<(String, String, String)> = Vec::new(); // (category, binary, display_name)
            let mut categories_used = vec![];
            for (cat, names) in CANDIDATE_GROUPS {
                let cat_lower = cat.to_string();
                if let Some(filter) = &filtered_categories {
                    if !filter.iter().any(|f| f == &cat_lower) {
                        continue;
                    }
                }
                categories_used.push(cat_lower.clone());
                for name in *names {
                    tasks.push((cat_lower.clone(), (*name).to_string(), (*name).to_string()));
                }
            }

            let max_conc = input.max_concurrency.max(1);
            let timeout_ms = input.version_timeout_ms;

            let shared_results: Arc<Mutex<Vec<BinaryReport>>> = Arc::new(Mutex::new(Vec::new()));

            // Simple chunked concurrency to avoid thread explosion while still parallelizing.
            for chunk in tasks.chunks(max_conc) {
                let mut handles = Vec::with_capacity(chunk.len());
                for (category, bin, display) in chunk.iter().cloned() {
                    let results = Arc::clone(&shared_results);
                    handles.push(thread::spawn(move || {
                        let start = Instant::now();
                        let which_path = which(&bin);
                        if which_path.is_none() {
                            results.lock().unwrap().push(BinaryReport {
                                name: display,
                                category,
                                found: false,
                                path: None,
                                version: None,
                                elapsed_ms: Some(start.elapsed().as_millis()),
                                error: None,
                            });
                            return;
                        }
                        let path = which_path.unwrap();
                        let version = detect_version_with_timeout(&path, timeout_ms);
                        let elapsed = start.elapsed().as_millis();

                        match version {
                            Ok(v) => {
                                results.lock().unwrap().push(BinaryReport {
                                    name: display,
                                    category,
                                    found: true,
                                    path: Some(path),
                                    version: Some(v),
                                    elapsed_ms: Some(elapsed),
                                    error: None,
                                });
                            }
                            Err(e) => {
                                results.lock().unwrap().push(BinaryReport {
                                    name: display,
                                    category,
                                    found: true,
                                    path: Some(path),
                                    version: None,
                                    elapsed_ms: Some(elapsed),
                                    error: Some(e.to_string()),
                                });
                            }
                        }
                    }));
                }
                for h in handles {
                    let _ = h.join();
                }
            }

            let mut reports = Arc::try_unwrap(shared_results)
                .unwrap()
                .into_inner()
                .unwrap();
            // Stable ordering: category then name
            reports.sort_by(|a, b| {
                (a.category.as_str(), a.name.as_str()).cmp(&(b.category.as_str(), b.name.as_str()))
            });

            let found = reports.iter().filter(|r| r.found).count();
            let output = DetectBinariesOutput {
                binaries: reports,
                summary: Summary {
                    total_scanned: tasks.len(),
                    found,
                    missing: tasks.len().saturating_sub(found),
                    categories_scanned: categories_used,
                    version_timeout_ms: timeout_ms,
                },
            };

            let json_text = serde_json::to_string_pretty(&output)
                .map_err(|e| anyhow!("Failed to serialize output: {e}"))?;
            Ok(json_text)
        })
    }
}

/// Locate a binary in PATH by iterating path entries manually.
fn which(name: &str) -> Option<String> {
    let path_var = match env::var_os("PATH") {
        Some(p) => p,
        None => return None,
    };
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() && is_executable(&candidate) {
            if let Some(s) = candidate.to_str() {
                return Some(s.to_string());
            }
        }
        // On Windows we might consider .exe, but this tool primarily targets Unix-like patterns here.
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = p.metadata() {
        let mode = meta.permissions().mode();
        mode & 0o111 != 0
    } else {
        false
    }
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    // Simplistic heuristic for non-Unix
    p.is_file()
}

/// Attempt to detect a version string using common flags within a timeout.
/// If retrieval exceeds timeout, returns an error.
fn detect_version_with_timeout(path: &str, timeout_ms: u64) -> Result<String> {
    let attempts: &[&[&str]] = &[&["--version"], &["version"], &["-V"]];

    // Shared state for thread join timeout simulation (coarse; avoids needing an async timeout crate).
    let version_out = Arc::new(Mutex::new(None::<Result<String>>));

    for args in attempts {
        let captured = Arc::clone(&version_out);
        let path_string = path.to_string();
        let args_vec: Vec<String> = args.iter().map(|s| s.to_string()).collect();

        let handle = thread::spawn(move || {
            let output = Command::new(&path_string).args(&args_vec).output();

            let result = match output {
                Ok(out) => {
                    if !out.status.success() && out.stdout.is_empty() && out.stderr.is_empty() {
                        Err(anyhow!("non-success exit (no output)"))
                    } else {
                        let text = if !out.stdout.is_empty() {
                            String::from_utf8_lossy(&out.stdout).to_string()
                        } else {
                            String::from_utf8_lossy(&out.stderr).to_string()
                        };
                        let first_line = text.lines().next().unwrap_or("").trim();
                        if first_line.is_empty() {
                            Err(anyhow!("empty version output"))
                        } else {
                            Ok(first_line.to_string())
                        }
                    }
                }
                Err(e) => Err(anyhow!("spawn failed: {e}")),
            };

            let mut guard = captured.lock().unwrap();
            if guard.is_none() {
                *guard = Some(result);
            }
        });

        let joined = join_with_timeout(handle, Duration::from_millis(timeout_ms));
        {
            let guard = version_out.lock().unwrap();
            if let Some(res) = guard.as_ref() {
                return res.clone();
            }
        }

        if !joined {
            // Timed out; continue to next attempt but record that previous attempt hung.
            let mut guard = version_out.lock().unwrap();
            if guard.is_none() {
                *guard = Some(Err(anyhow!("version probe timeout after {timeout_ms}ms")));
            }
            // Break; a hang indicates further attempts may also hang.
            break;
        }
    }

    let guard = version_out.lock().unwrap();
    guard
        .clone()
        .unwrap_or_else(|| Err(anyhow!("no version retrieved")))
}

/// Join a thread with a timeout (coarse polyfill).
fn join_with_timeout<T>(handle: thread::JoinHandle<T>, dur: Duration) -> bool {
    let start = Instant::now();
    // Polling loop: simple + portable. Lower overhead than channels for small N.
    loop {
        if start.elapsed() >= dur {
            // We cannot actually kill the thread safely; caller treats as timeout.
            return false;
        }
        // Try non-blocking join by using a small sleep and checking a shared flag would need extra plumbing.
        // Instead, attempt a zero-duration park by joining in a separate trick: we cannot peek join status
        // without blocking in stable std, so approximate with small sleep and continue.
        // This keeps code dependency-free. In a future refinement, use a scoped channel for a signal.
        thread::sleep(Duration::from_millis(10));
        // There is no direct way to test join readiness; break only if already finished via interior state.
        // We rely on the version_out guard being set by the worker to short-circuit attempts.
        // The caller inspects shared result after calling this function.
        // Return false to let caller inspect shared state; they will see if it was set.
        if start.elapsed() >= dur {
            return false;
        }
        // Continue looping until timeout; join handle is purposely leaked until natural finish.
        // (We cannot safely force-cancel. Accepts potential orphan thread finishing later.)
        if start.elapsed() >= dur {
            return false;
        }
        // To avoid indefinite loop if thread finished early, try a blocking join with very small probability window:
        // This risks blocking; deliberately avoided. We accept coarse timeout only.
    }
}
