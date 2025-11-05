use crate::{AgentTool, ToolCallEventStream};
use agent_client_protocol::ToolKind;
use anyhow::{Result, anyhow};
use gpui::{App, SharedString, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

/// Structure parsed from settings.json -> "detect_binaries"
#[derive(Debug, Default, Deserialize)]
struct DetectBinariesSettingsFile {
    #[serde(default)]
    add_groups: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    add_binaries: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    exclude_groups: Vec<String>,
    #[serde(default)]
    exclude_binaries: Vec<String>,
    #[serde(default)]
    include_only_groups: Option<Vec<String>>,
}

fn load_settings_file() -> DetectBinariesSettingsFile {
    // Best-effort: look for .zed/settings.json in current working directory.
    // If not found or invalid, return defaults silently.
    let candidate_paths = [
        ".zed/settings.json",
        "settings.json", // fallback if user keeps a flat project settings file
    ];
    for p in candidate_paths {
        if let Ok(data) = fs::read_to_string(p) {
            if let Ok(root) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(obj) = root.get("detect_binaries") {
                    if let Ok(cfg) =
                        serde_json::from_value::<DetectBinariesSettingsFile>(obj.clone())
                    {
                        return cfg;
                    }
                }
            }
        }
    }
    DetectBinariesSettingsFile::default()
}

/// Input for the `detect_binaries` tool.
/// `filter_categories` (if provided) limits scanning to those category identifiers (case-insensitive).
/// `max_concurrency` is a soft upper bound; values < 1 are treated as 1.
/// `version_timeout_ms` applies per attempt (flag) for each binary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DetectBinariesToolInput {
    #[serde(default)]
    filter_categories: Option<Vec<String>>,
    #[serde(default = "default_max_concurrency")]
    max_concurrency: usize,
    #[serde(default = "default_version_timeout_ms")]
    version_timeout_ms: u64,
}

fn default_max_concurrency() -> usize {
    12
}

fn default_version_timeout_ms() -> u64 {
    1500
}

/// Category identifiers and associated candidate binary names (base set).
/// Order matters (stable output).
const BASE_CANDIDATE_GROUPS: &[(&str, &[&str])] = &[
    (
        "package_managers",
        &[
            "pnpm", "yarn", "npm", "pip", "pipx", "uv", "uvx", "poetry", "cargo", "rustup", "dnf", "apt", "pacman", "zypper", "snap", "flatpak",
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

/// Build the effective groups list applying settings.json overrides.
fn build_effective_groups(
    cfg: &DetectBinariesSettingsFile,
    filtered_categories: &Option<Vec<String>>,
) -> Vec<(String, Vec<String>)> {
    // Start with base groups
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (g, bins) in BASE_CANDIDATE_GROUPS {
        map.entry((*g).to_string())
            .or_default()
            .extend(bins.iter().map(|b| (*b).to_string()));
    }

    // Add whole new groups
    for (g, bins) in &cfg.add_groups {
        map.entry(g.to_lowercase())
            .or_default()
            .extend(bins.iter().map(|b| b.to_string()));
    }

    // Add binaries to existing groups
    for (g, bins) in &cfg.add_binaries {
        map.entry(g.to_lowercase())
            .or_default()
            .extend(bins.iter().map(|b| b.to_string()));
    }

    // Exclude binaries
    let exclude_bin: BTreeSet<String> = cfg
        .exclude_binaries
        .iter()
        .map(|b| b.to_lowercase())
        .collect();

    // Exclude groups
    let exclude_groups: BTreeSet<String> = cfg
        .exclude_groups
        .iter()
        .map(|g| g.to_lowercase())
        .collect();

    // If include_only_groups present, treat it as hard filter (after excludes)
    let include_only: Option<BTreeSet<String>> = cfg
        .include_only_groups
        .as_ref()
        .map(|v| v.iter().map(|g| g.to_lowercase()).collect());

    // Apply explicit input filter_categories (provided via tool input) as secondary filter
    let input_filter: Option<BTreeSet<String>> = filtered_categories
        .as_ref()
        .map(|v| v.iter().map(|g| g.to_lowercase()).collect());

    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (group, bins) in map {
        let gl = group.to_lowercase();
        if exclude_groups.contains(&gl) {
            continue;
        }
        if let Some(only) = &include_only {
            if !only.contains(&gl) {
                continue;
            }
        }
        if let Some(filt) = &input_filter {
            if !filt.contains(&gl) {
                continue;
            }
        }
        let final_bins: Vec<String> = bins
            .into_iter()
            .filter(|b| !exclude_bin.contains(&b.to_lowercase()))
            .collect();
        if !final_bins.is_empty() {
            out.push((group, final_bins));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

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
        let authorize = event_stream.authorize(self.initial_title(Ok(input.clone()), cx), cx);

        cx.spawn(async move |_cx| {
            authorize.await?;

            let filtered_categories: Option<Vec<String>> = input
                .filter_categories
                .as_ref()
                .map(|v| v.iter().map(|s| s.to_lowercase()).collect());

            let cfg = load_settings_file();
            let effective_groups = build_effective_groups(&cfg, &filtered_categories);

            let mut tasks: Vec<(String, String, String)> = Vec::new();
            let mut categories_used = Vec::new();

            for (cat, names_vec) in effective_groups {
                let cat_lower = cat.to_string();
                categories_used.push(cat_lower.clone());
                for name in names_vec {
                    tasks.push((cat_lower.clone(), name.clone(), name));
                }
            }

            let max_conc = input.max_concurrency.max(1);
            let timeout_ms = input.version_timeout_ms;

            let shared_results: Arc<Mutex<Vec<BinaryReport>>> = Arc::new(Mutex::new(Vec::new()));

            for chunk in tasks.chunks(max_conc) {
                let mut handles = Vec::with_capacity(chunk.len());
                for (category, bin, display) in chunk.iter().cloned() {
                    let results = Arc::clone(&shared_results);
                    handles.push(thread::spawn(move || {
                        let start = Instant::now();
                        let paths = which_all(&bin);
                        if paths.is_empty() {
                            if let Ok(mut vec) = results.lock() {
                                vec.push(BinaryReport {
                                    name: display,
                                    category,
                                    found: false,
                                    path: None,
                                    version: None,
                                    elapsed_ms: Some(start.elapsed().as_millis()),
                                    error: None,
                                });
                            }
                            return;
                        }

                        let path_field = if paths.len() > 1 {
                            Some(paths.join(";"))
                        } else {
                            None
                        };

                        let probe_path = &paths[0];
                        let version_result = detect_version_with_timeout(probe_path, timeout_ms);
                        let elapsed = start.elapsed().as_millis();

                        let mut push_report =
                            |found: bool, version: Option<String>, error: Option<String>| {
                                if let Ok(mut vec) = results.lock() {
                                    vec.push(BinaryReport {
                                        name: display.clone(),
                                        category: category.clone(),
                                        found,
                                        path: path_field.clone(),
                                        version,
                                        elapsed_ms: Some(elapsed),
                                        error,
                                    });
                                }
                            };

                        match version_result {
                            Ok(v) => push_report(true, Some(v), None),
                            Err(e) => push_report(true, None, Some(e.to_string())),
                        }
                    }));
                }
                for h in handles {
                    let _ = h.join();
                }
            }

            let mut reports = match Arc::try_unwrap(shared_results) {
                Ok(mutex) => match mutex.into_inner() {
                    Ok(vec) => vec,
                    Err(_) => Vec::new(),
                },
                Err(_) => Vec::new(),
            };

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

fn which_all(name: &str) -> Vec<String> {
    let mut matches = Vec::new();
    let path_var = match env::var_os("PATH") {
        Some(p) => p,
        None => return matches,
    };
    for dir in env::split_paths(&path_var) {
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
    if let Ok(meta) = p.metadata() {
        let mode = meta.permissions().mode();
        mode & 0o111 != 0
    } else {
        false
    }
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

fn detect_version_with_timeout(path: &str, timeout_ms: u64) -> Result<String> {
    let attempts: &[&[&str]] = &[&["--version"], &["version"], &["-V"]];
    let mut last_err: Option<anyhow::Error> = None;

    for args in attempts {
        match probe_once(path, args, timeout_ms) {
            Ok(line) => return Ok(line),
            Err(e) => {
                last_err = Some(e);
                if last_err
                    .as_ref()
                    .map(|er| er.to_string().contains("timeout"))
                    .unwrap_or(false)
                {
                    break;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("no version retrieved")))
}

fn probe_once(path: &str, args: &[&str], timeout_ms: u64) -> Result<String> {
    let (tx, rx) = mpsc::channel();

    let path_string = path.to_string();
    let args_vec: Vec<String> = args.iter().map(|s| s.to_string()).collect();

    thread::spawn(move || {
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
        let _ = tx.send(result);
    });

    match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(anyhow!("version probe timeout after {timeout_ms}ms"))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow!("version probe worker disconnected"))
        }
    }
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write, os::unix::fs::PermissionsExt};

    fn make_script(body: &str) -> String {
        let uniq = format!(
            "detect_bin_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(uniq);
        {
            let mut f = fs::File::create(&path).expect("create script");
            writeln!(f, "#!/bin/sh").unwrap();
            f.write_all(body.as_bytes()).unwrap();
        }
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn version_detection_success() {
        let script = make_script("echo MyTool 1.2.3");
        let v = detect_version_with_timeout(&script, 500).expect("should get version");
        assert!(v.contains("MyTool"));
        assert!(v.contains("1.2.3"));
    }

    #[test]
    fn version_detection_timeout() {
        let script = make_script("sleep 2; echo LateOutput 9.9.9");
        let err = detect_version_with_timeout(&script, 200).expect_err("should timeout");
        assert!(err.to_string().contains("timeout"));
    }

    #[test]
    fn version_detection_fallback_attempt() {
        // First attempt (--version) exits non-zero with no output, second (version) succeeds.
        let script = make_script(
            r#"
if [ "$1" = "--version" ]; then
  exit 1
elif [ "$1" = "version" ]; then
  echo FallbackTool 0.9.0
else
  echo Unexpected $1
fi
"#,
        );
        let v = detect_version_with_timeout(&script, 800).expect("fallback should succeed");
        assert!(v.contains("FallbackTool"));
        assert!(v.contains("0.9.0"));
    }
}
