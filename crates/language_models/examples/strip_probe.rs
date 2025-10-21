use std::collections::{HashMap, HashSet};
use std::env;

use anyhow::Result;
use http_client::HttpClient;
use language_models::provider::azure_foundry::{
    DeploymentProbeResult, probe_deployments_existence,
};
use reqwest_client::ReqwestClient;

/// Strip a trailing -YYYY-MM-DD suffix from a model name to derive a base deployment name.
///
/// Examples:
/// - "gpt-5-2025-08-07" -> "gpt-5"
/// - "gpt-5-mini-2025-08-07" -> "gpt-5-mini"
/// - "gpt-5-chat-2025-08-07" -> "gpt-5-chat"
fn base_model_name(name: &str) -> String {
    // Strip a trailing "-YYYY-MM-DD" suffix across multiple hyphens.
    // Instead of splitting on the last hyphen, detect the full date pattern at the end.
    let bytes = name.as_bytes();
    if bytes.len() >= 11 {
        let idx = bytes.len() - 11;
        // Pattern: '-' + 4 digits + '-' + 2 digits + '-' + 2 digits
        let is_date_suffix = bytes[idx] == b'-'
            && bytes[idx + 1..idx + 5].iter().all(|b| b.is_ascii_digit())
            && bytes[idx + 5] == b'-'
            && bytes[idx + 6..idx + 8].iter().all(|b| b.is_ascii_digit())
            && bytes[idx + 8] == b'-'
            && bytes[idx + 9..idx + 11].iter().all(|b| b.is_ascii_digit());
        if is_date_suffix {
            return name[..idx].to_string();
        }
    }
    name.to_string()
}

/// Read a comma-separated environment var into a Vec<String>.
fn read_csv_env(name: &str) -> Option<Vec<String>> {
    env::var(name).ok().map(|s| {
        s.split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // Endpoint and credentials (api-key header expected).
    // AZURE_ENDPOINT: e.g. "https://<resource>.cognitiveservices.azure.com"
    // AZURE_API_KEY: your key/token for the resource.
    let api_url = env::var("AZURE_ENDPOINT")
        .unwrap_or_else(|_| "https://torst-m74j7gzn-eastus2.cognitiveservices.azure.com".into());
    let api_key = match env::var("AZURE_API_KEY") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("Missing AZURE_API_KEY; set it to run this example.");
            return Ok(());
        }
    };
    let api_version = env::var("AZURE_FOUNDRY_API_VERSION").ok();

    // Candidate models (override via CANDIDATE_MODELS="a,b,c")
    let candidates = read_csv_env("CANDIDATE_MODELS").unwrap_or_else(|| {
        vec![
            "gpt-5-2025-08-07".to_string(),
            "gpt-5-mini-2025-08-07".to_string(),
            "gpt-5-chat-2025-08-07".to_string(),
            "gpt-1-2000-01-01".to_string(),
        ]
    });

    println!("Candidate models:");
    for m in &candidates {
        println!(" - {}", m);
    }

    // Derive base names by stripping -YYYY-MM-DD.
    let mut bases_ordered = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    let mut model_to_base = HashMap::<String, String>::new();

    for m in &candidates {
        let base = base_model_name(m);
        model_to_base.insert(m.clone(), base.clone());
        if seen.insert(base.clone()) {
            bases_ordered.push(base);
        }
    }

    println!("\nDerived base deployment names:");
    for b in &bases_ordered {
        println!(" - {}", b);
    }

    // HTTP client.
    let http_client = ReqwestClient::user_agent("azure-foundry-strip-probe")
        .unwrap_or_else(|_| ReqwestClient::new());
    let client_ref: &dyn HttpClient = &http_client;

    // Probe base deployments concurrently.
    let base_refs: Vec<&str> = bases_ordered.iter().map(|s| s.as_str()).collect();
    println!("\nProbing base deployments at {} ...", api_url);
    let results = probe_deployments_existence(
        client_ref,
        &api_url,
        &api_key,
        &base_refs,
        api_version.as_deref(),
    )
    .await?;

    println!("\nDeployment probe results:");
    let mut base_status = HashMap::<String, DeploymentProbeResult>::new();
    for (name, status) in results {
        let status_str = match &status {
            DeploymentProbeResult::Found => "Found".to_string(),
            DeploymentProbeResult::AccessDenied => "AccessDenied".to_string(),
            DeploymentProbeResult::NotFound => "NotFound".to_string(),
            DeploymentProbeResult::ServerError(code, _) => format!("ServerError {}", code),
            DeploymentProbeResult::Other(code, _) => format!("Other {}", code),
        };
        println!(" - {} -> {}", name, status_str);
        base_status.insert(name, status);
    }

    println!("\nModel -> base mapping and base status:");
    for m in &candidates {
        let base = model_to_base
            .get(m)
            .cloned()
            .unwrap_or_else(|| base_model_name(m));
        let status = base_status.get(&base);
        let status_str = match status {
            Some(DeploymentProbeResult::Found) => "Found",
            Some(DeploymentProbeResult::AccessDenied) => "AccessDenied",
            Some(DeploymentProbeResult::NotFound) => "NotFound",
            Some(DeploymentProbeResult::ServerError(code, _)) => {
                println!(" - {} => {} (ServerError {})", m, base, code);
                continue;
            }
            Some(DeploymentProbeResult::Other(code, _)) => {
                println!(" - {} => {} (Other {})", m, base, code);
                continue;
            }
            None => "unknown",
        };
        println!(" - {} => {} ({})", m, base, status_str);
    }

    // Specific expectations:

    // "gpt-5-2025-08-07" strips to "gpt-5" and should be Found (as confirmed by probe).
    if candidates.iter().any(|m| m == "gpt-5-2025-08-07") {
        let base = base_model_name("gpt-5-2025-08-07");
        let status = base_status.get(&base);
        println!(
            "\nExpectation check: '{}' strips to '{}', status: {}",
            "gpt-5-2025-08-07",
            base,
            match status {
                Some(DeploymentProbeResult::Found) => "Found",
                Some(DeploymentProbeResult::AccessDenied) => "AccessDenied",
                Some(DeploymentProbeResult::NotFound) => "NotFound",
                Some(DeploymentProbeResult::ServerError(code, _)) =>
                    return Ok(println!("ServerError {}", code)),
                Some(DeploymentProbeResult::Other(code, _)) =>
                    return Ok(println!("Other {}", code)),
                None => "unknown",
            }
        );
    }

    // "gpt-1-2000-01-01" strips to "gpt-1" and should be NotFound (nonexistent legacy/deprecated).
    if candidates.iter().any(|m| m == "gpt-1-2000-01-01") {
        let base = base_model_name("gpt-1-2000-01-01");
        let status = base_status.get(&base);
        println!(
            "\nExpectation check: '{}' strips to '{}', status: {} (expected NotFound)",
            "gpt-1-2000-01-01",
            base,
            match status {
                Some(DeploymentProbeResult::Found) => "Found",
                Some(DeploymentProbeResult::AccessDenied) => "AccessDenied",
                Some(DeploymentProbeResult::NotFound) => "NotFound",
                Some(DeploymentProbeResult::ServerError(code, _)) =>
                    return Ok(println!("ServerError {}", code)),
                Some(DeploymentProbeResult::Other(code, _)) =>
                    return Ok(println!("Other {}", code)),
                None => "unknown",
            }
        );
        if !matches!(status, Some(DeploymentProbeResult::NotFound)) {
            println!("Warning: expected NotFound for '{}'", "gpt-1-2000-01-01");
        }
    }

    Ok(())
}
