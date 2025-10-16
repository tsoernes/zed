use std::env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Semaphore;
use tokio::time::timeout;

use http_client::HttpClient;
use reqwest_client::ReqwestClient;

use language_models::provider::azure_foundry::{
    DeploymentProbeResult, probe_deployments_existence,
};

/// Small helper to render a DeploymentProbeResult into a compact string and optional detail.
fn render_probe_result(r: &DeploymentProbeResult) -> (String, Option<String>) {
    match r {
        DeploymentProbeResult::Found => ("Found".to_string(), None),
        DeploymentProbeResult::NotFound => ("NotFound".to_string(), None),
        DeploymentProbeResult::AccessDenied => ("AccessDenied".to_string(), None),
        DeploymentProbeResult::ServerError(code, body) => (
            "ServerError".to_string(),
            Some(format!("{} {}", code, body)),
        ),
        DeploymentProbeResult::Other(code, body) => {
            ("Other".to_string(), Some(format!("{} {}", code, body)))
        }
    }
}

/// Example test program that probes a list of candidate deployment names concurrently,
/// using short per-probe timeouts and a concurrency limit.
///
/// Usage:
///   export AZURE_ENDPOINT="https://<your-endpoint>"
///   export AZURE_API_KEY="<your-key-or-token>"
///   export CANDIDATE_NAMES="gpt-5,gpt-4o,gpt-35-turbo,my-deploy"   # optional
///   cargo run -p language_models --example probe_deployments
#[tokio::main]
async fn main() -> Result<()> {
    // Read configuration from environment.
    let api_url = env::var("AZURE_ENDPOINT")
        .unwrap_or_else(|_| "https://torst-m74j7gzn-eastus2.cognitiveservices.azure.com".into());
    let api_key = env::var("AZURE_API_KEY").expect("AZURE_API_KEY must be set to run this example");
    let api_version = env::var("AZURE_API_VERSION").ok();

    // Candidate names can be provided via CANDIDATE_NAMES (comma-separated).
    // Otherwise use a sensible default list to demonstrate behavior.
    let candidates_env = env::var("CANDIDATE_NAMES").ok();
    let default_candidates = vec![
        "gpt-5",
        "gpt-5-mini",
        "gpt-4o",
        "gpt-4o-mini",
        "gpt-4",
        "gpt-35-turbo",
        "deepseek-r1",
        "davinci",
    ];
    let candidates: Vec<String> = if let Some(s) = candidates_env {
        s.split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect()
    } else {
        default_candidates
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    };

    if candidates.is_empty() {
        eprintln!("No candidate names provided; nothing to probe.");
        return Ok(());
    }

    // Concurrency and timeout configuration
    let concurrency: usize = env::var("PROBE_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let per_probe_timeout_secs: u64 = env::var("PROBE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);

    println!("Endpoint: {}", api_url);
    println!("API version (optional): {:?}", api_version);
    println!(
        "Probing {} candidate names with concurrency={} timeout={}s",
        candidates.len(),
        concurrency,
        per_probe_timeout_secs
    );

    // Construct HTTP client used by the crate.
    // Use a simple user agent to identify the probe.
    let reqwest_client = ReqwestClient::user_agent("azure-foundry-probe-examples")
        .unwrap_or_else(|_| ReqwestClient::new());
    let client: Arc<dyn HttpClient> = Arc::new(reqwest_client);

    // Semaphore to limit concurrency
    let sem = Arc::new(Semaphore::new(concurrency));

    // Launch tasks for each candidate
    let mut handles = Vec::with_capacity(candidates.len());
    for name in candidates.into_iter() {
        let client = client.clone();
        let api_url = api_url.clone();
        let api_key = api_key.clone();
        let api_version = api_version.clone();
        let sem = sem.clone();
        let name_clone = name.clone();
        let timeout_dur = Duration::from_secs(per_probe_timeout_secs);

        let handle = tokio::spawn(async move {
            // Acquire permit (async)
            let _permit = sem.acquire().await.unwrap();

            // We'll call the helper for a single-candidate slice
            let fut = probe_deployments_existence(
                client.as_ref(),
                &api_url,
                &api_key,
                &[name_clone.as_str()],
                api_version.as_deref(),
            );

            match timeout(timeout_dur, fut).await {
                Ok(Ok(results)) => {
                    // results is Vec<(String, DeploymentProbeResult)>, but we asked for single candidate
                    if let Some((candidate_name, res)) = results.into_iter().next() {
                        let (label, details) = render_probe_result(&res);
                        (candidate_name, Ok((label, details)))
                    } else {
                        (name_clone, Err("no result returned".to_string()))
                    }
                }
                Ok(Err(e)) => (name_clone, Err(format!("probe failed: {}", e))),
                Err(_) => (
                    name_clone,
                    Err(format!("timed out after {}s", timeout_dur.as_secs())),
                ),
            }
        });

        handles.push(handle);
    }

    // Collect results
    let mut collected = Vec::new();
    for h in handles {
        match h.await {
            Ok((name, Ok((label, details)))) => {
                collected.push((name, label, details));
            }
            Ok((name, Err(err))) => {
                collected.push((name, "Error".to_string(), Some(err)));
            }
            Err(join_err) => {
                collected.push((
                    "<task-join-failed>".to_string(),
                    "Error".to_string(),
                    Some(format!("task join error: {}", join_err)),
                ));
            }
        }
    }

    // Print a friendly table
    println!();
    println!(
        "{:<30} {:<12} {}",
        "Candidate", "Result", "Details (truncated)"
    );
    println!("{}", "-".repeat(80));
    for (name, label, details) in collected {
        let short = details
            .as_ref()
            .map(|d| {
                if d.len() > 120 {
                    format!("{}...", &d[..120])
                } else {
                    d.clone()
                }
            })
            .unwrap_or_default();
        println!("{:<30} {:<12} {}", name, label, short);
    }

    Ok(())
}
