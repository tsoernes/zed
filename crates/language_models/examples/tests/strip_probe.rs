use std::collections::{HashMap, HashSet};
use std::env;

use anyhow::Result;
use http_client::HttpClient;
use language_models::provider::azure_foundry::{
    DeploymentProbeResult, probe_deployments_existence,
};
use reqwest_client::ReqwestClient;

/// Strip a trailing -YYYY-MM-DD suffix from a model name to derive a base deployment name.
/// Examples:
/// - "gpt-5-2025-08-07" -> "gpt-5"
/// - "gpt-5-mini-2025-08-07" -> "gpt-5-mini"
/// - "gpt-5-chat-2025-08-07" -> "gpt-5-chat"
fn base_model_name(name: &str) -> String {
    if let Some((prefix, suffix)) = name.rsplit_once('-') {
        let is_date = suffix.len() == 10
            && suffix.chars().enumerate().all(|(i, c)| match i {
                4 | 7 => c == '-',
                _ => c.is_ascii_digit(),
            });
        if is_date {
            return prefix.to_string();
        }
    }
    name.to_string()
}

/// Read a comma-separated env var into a Vec<String>.
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
    // Endpoint and credentials.
    // AZURE_ENDPOINT: e.g., "https://<resource-name>.cognitiveservices.azure.com"
    // AZURE_API_KEY: key or token (api-key header in most Foundry endpoints)
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

    // Candidate models to test; defaults focus on gpt-5 variants with date suffix.
    let candidates = read_csv_env("CANDIDATE_MODELS").unwrap_or_else(|| {
        vec![
            "gpt-5-2025-08-07".to_string(),
            "gpt-5-mini-2025-08-07".to_string(),
            "gpt-5-chat-2025-08-07".to_string(),
        ]
    });

    println!("Candidate models:");
    for m in &candidates {
        println!(" - {}", m);
    }

    // Derive base deployment names by stripping -YYYY-MM-DD suffix if present.
    let mut bases_ordered = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    let mut map_model_to_base = HashMap::<String, String>::new();

    for m in &candidates {
        let base = base_model_name(m);
        map_model_to_base.insert(m.clone(), base.clone());
        if seen.insert(base.clone()) {
            bases_ordered.push(base);
        }
    }

    println!("\nDerived base deployment names:");
    for b in &bases_ordered {
        println!(" - {}", b);
    }

    // Build HTTP client
    let reqwest_client = ReqwestClient::user_agent("azure-foundry-strip-probe")
        .unwrap_or_else(|_| ReqwestClient::new());
    let client_ref: &dyn HttpClient = &reqwest_client;

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

    // Present results.
    println!("\nDeployment probe results:");
    let mut base_status = HashMap::<String, DeploymentProbeResult>::new();
    for (name, status) in results {
        let status_str = match &status {
            DeploymentProbeResult::Found => "Found",
            DeploymentProbeResult::AccessDenied => "AccessDenied",
            DeploymentProbeResult::NotFound => "NotFound",
            DeploymentProbeResult::ServerError(code, _) => {
                // Body trimmed in the probing function logs; keep it short here.
                // We only show the code to avoid noisy output.
                // If you need details, run with RUST_LOG=debug.
                println!(" - {} -> ServerError {}", name, code);
                base_status.insert(name, status);
                continue;
            }
            DeploymentProbeResult::Other(code, _) => {
                println!(" - {} -> Other {}", name, code);
                base_status.insert(name, status);
                continue;
            }
        };
        println!(" - {} -> {}", name, status_str);
        base_status.insert(name, status);
    }

    // Check that date-suffixed models map to base names that exist.
    println!("\nModel -> base mapping and base status:");
    for m in &candidates {
        let base = map_model_to_base
            .get(m)
            .cloned()
            .unwrap_or_else(|| base_model_name(m));
        let status = base_status.get(&base);
        match status {
            Some(DeploymentProbeResult::Found) | Some(DeploymentProbeResult::AccessDenied) => {
                println!(" - {} => {} (OK)", m, base);
            }
            Some(DeploymentProbeResult::NotFound) => {
                println!(" - {} => {} (NotFound)", m, base);
            }
            Some(DeploymentProbeResult::ServerError(code, _)) => {
                println!(" - {} => {} (ServerError {})", m, base, code);
            }
            Some(DeploymentProbeResult::Other(code, _)) => {
                println!(" - {} => {} (Other {})", m, base, code);
            }
            None => {
                println!(" - {} => {} (no status)", m, base);
            }
        }
    }

    // Specific check for gpt-5-2025-08-07 -> gpt-5
    if candidates.iter().any(|m| m == "gpt-5-2025-08-07") {
        let base = base_model_name("gpt-5-2025-08-07");
        let status = base_status.get(&base);
        println!(
            "\nExpectation check: 'gpt-5-2025-08-07' strips to '{}', status: {}",
            base,
            match status {
                Some(DeploymentProbeResult::Found) => "Found",
                Some(DeploymentProbeResult::AccessDenied) => "AccessDenied",
                Some(DeploymentProbeResult::NotFound) => "NotFound",
                Some(DeploymentProbeResult::ServerError(code, _)) => {
                    // Return code only
                    return Ok(println!("ServerError {}", code));
                }
                Some(DeploymentProbeResult::Other(code, _)) => {
                    return Ok(println!("Other {}", code));
                }
                None => "unknown",
            }
        );
    }

    Ok(())
}
