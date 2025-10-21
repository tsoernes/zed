use std::env;
use std::sync::Arc;

use anyhow::Result;

use http_client::HttpClient;
use reqwest_client::ReqwestClient;

use language_models::provider::azure_foundry::{discover_models, probe_use_model};

#[tokio::main]
async fn main() -> Result<()> {
    // Read configuration from environment (set these before running)
    let api_url = env::var("AZURE_ENDPOINT")
        .unwrap_or_else(|_| "https://torst-m74j7gzn-eastus2.cognitiveservices.azure.com".into());
    let api_key = env::var("AZURE_API_KEY").expect("AZURE_API_KEY must be set to run this example");
    let api_version = env::var("AZURE_API_VERSION").ok();
    let deployment = env::var("AZURE_DEPLOYMENT").ok();

    // Create a concrete HTTP client used by the crate and wrap it as a trait object.
    let client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::new());

    // If a deployment is explicitly provided, prefer calling the deployment-specific
    // endpoint directly (skip discovery). This mirrors the curl behavior that worked.
    let prompt = "I am going to Paris, what should I see?";

    if let Some(dep) = deployment.clone() {
        // Use the deployment name as the model name when probing deployment-specific endpoints.
        let model_to_probe = dep.clone();
        println!(
            "Deployment specified: {}. Preferring deployment-specific probe at {} ...",
            dep, api_url
        );

        match probe_use_model(
            client.as_ref(),
            &api_url,
            &api_key,
            &model_to_probe,
            Some(dep.clone()),
            api_version.as_deref(),
            prompt,
        )
        .await
        {
            Ok(body) => {
                println!(
                    "Deployment-specific probe succeeded. Raw response body:\n{}",
                    body
                );
            }
            Err(e) => {
                eprintln!("Deployment-specific probe failed: {:#}", e);
            }
        }
    } else {
        // No deployment specified — fall back to discovery and then probe the first discovered model.
        println!(
            "No deployment specified; discovering models at: {}",
            api_url
        );
        match discover_models(client.as_ref(), &api_url, &api_key, api_version.as_deref()).await {
            Ok(models) => {
                if models.is_empty() {
                    println!("No models discovered; falling back to default model 'gpt-5'.");
                    let model_to_probe = "gpt-5".to_string();
                    match probe_use_model(
                        client.as_ref(),
                        &api_url,
                        &api_key,
                        &model_to_probe,
                        None,
                        api_version.as_deref(),
                        prompt,
                    )
                    .await
                    {
                        Ok(body) => println!("Probe succeeded. Raw response body:\n{}", body),
                        Err(e) => eprintln!("probe_use_model failed: {:#}", e),
                    }
                } else {
                    println!("Discovered {} model(s):", models.len());
                    for m in &models {
                        println!(
                            " - {} (display: {:?}, max_tokens: {})",
                            m.name, m.display_name, m.max_tokens
                        );
                    }

                    // Prefer the first discovered model name for a quick probe.
                    let model_to_probe = models[0].name.clone();
                    println!(
                        "Probing discovered model '{}' at {} ...",
                        model_to_probe, api_url
                    );

                    match probe_use_model(
                        client.as_ref(),
                        &api_url,
                        &api_key,
                        &model_to_probe,
                        None,
                        api_version.as_deref(),
                        prompt,
                    )
                    .await
                    {
                        Ok(body) => println!("Probe succeeded. Raw response body:\n{}", body),
                        Err(e) => eprintln!("probe_use_model failed: {:#}", e),
                    }
                }
            }
            Err(e) => {
                eprintln!("discover_models failed: {:#}", e);
                // As a last resort, try probing a sensible default model name without deployment.
                let model_to_probe = "gpt-5".to_string();
                println!(
                    "Attempting probe with fallback model '{}' ...",
                    model_to_probe
                );
                match probe_use_model(
                    client.as_ref(),
                    &api_url,
                    &api_key,
                    &model_to_probe,
                    None,
                    api_version.as_deref(),
                    prompt,
                )
                .await
                {
                    Ok(body) => println!("Probe succeeded. Raw response body:\n{}", body),
                    Err(e) => eprintln!("probe_use_model failed: {:#}", e),
                }
            }
        }
    }

    Ok(())
}
