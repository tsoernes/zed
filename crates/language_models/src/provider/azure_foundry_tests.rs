/*
Integration test scaffolding for Azure AI Foundry provider.

This test performs three things:
1) Attempts autodiscovery of models from the configured Foundry endpoint.
2) Dedupe discovered models by name and limit the list to a small set.
3) For each model in the limited list, attempt a minimal probe call (Responses or chat/completions)
   and print the returned body (first 1000 chars) for inspection.

The test is safe to run in CI/local because it will skip when env vars are not present.
To run locally set:
  AZURE_ENDPOINT=https://...
  AZURE_API_KEY=...
*/

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use std::env;

    // Basic check that required environment variables are present. This is a
    // smoke test intended for manual/integration runs. It will be skipped when
    // the variables are absent.
    #[gpui::test]
    fn test_azure_foundry_env_present(_cx: &TestAppContext) {
        let endpoint = env::var("AZURE_ENDPOINT").ok();
        let api_key = env::var("AZURE_API_KEY").ok();

        if endpoint.is_none() || api_key.is_none() {
            eprintln!(
                "Skipping Azure Foundry env check: AZURE_ENDPOINT/AZURE_API_KEY not set (this is expected in many CI environments)"
            );
            return;
        }

        let endpoint = endpoint.unwrap();
        let api_key = api_key.unwrap();

        // Minimal validation of values (not exhaustive).
        assert!(
            endpoint.starts_with("https://"),
            "AZURE_ENDPOINT should start with https://"
        );
        assert!(
            !api_key.trim().is_empty(),
            "AZURE_API_KEY must not be empty"
        );
    }

    // Integration test: discover models, dedupe by name, probe limited set and print responses.
    // Also include a set of explicit deployment/model names (from the Foundry) and attempt probing
    // each one directly. The probe uses probe_use_model which tries Responses and chat/completions.
    #[gpui::test]
    fn test_discover_and_probe_models(_cx: &TestAppContext) {
        use futures::executor::block_on;
        use reqwest_client::ReqwestClient;
        use std::collections::HashSet;
        use std::sync::Arc;

        let endpoint = env::var("AZURE_ENDPOINT").ok();
        let api_key = env::var("AZURE_API_KEY").ok();

        if endpoint.is_none() || api_key.is_none() {
            eprintln!("Skipping Azure Foundry discovery/probe test: env vars not set");
            return;
        }

        let endpoint = endpoint.unwrap();
        let api_key = api_key.unwrap();

        // Create an HTTP client backed by reqwest (ReqwestClient implements http_client::HttpClient)
        let http_client = Arc::new(
            ReqwestClient::user_agent("azure-foundry-integration-test")
                .expect("failed to create http client"),
        );

        // 1) Autodiscover models using the provider helper (best-effort)
        let discovered = match block_on(crate::provider::azure_foundry::discover_models(
            http_client.as_ref(),
            &endpoint,
            &api_key,
            None,
        )) {
            Ok(models) => models,
            Err(err) => {
                eprintln!("Discovery failed: {}", err);
                Vec::new()
            }
        };

        // Build a deduplicated list of model names to probe. Start with discovered models.
        let mut seen = HashSet::new();
        let mut probe_models: Vec<String> = Vec::new();
        for m in discovered {
            if seen.insert(m.name.clone()) {
                probe_models.push(m.name.clone());
            }
        }

        // 2) Add explicit known deployment/model names from the Foundry (as provided).
        // These are added only if not already discovered; they help ensure we try the exact deployment ids.
        let explicit = vec![
            "DeepSeek-R1",
            "DeepSeek-R1-0528",
            "gpt-4o-east-US",
            "gpt-4o-mini",
            "gpt-4o-mini-transcribe",
            "gpt-4o-mini-tts",
            "gpt-4o-transcribe",
            "gpt-4o-transcribe-2",
            "gpt-5",
            "gpt-5-chat",
            "gpt-5-codex",
            "gpt-5-mini",
            "gpt-realtime",
            "model-router",
        ];

        for name in explicit {
            if seen.insert(name.to_string()) {
                probe_models.push(name.to_string());
            }
        }

        if probe_models.is_empty() {
            eprintln!(
                "No models to probe (discovery returned none and no explicit models available)"
            );
            return;
        }

        // Limit the number of probes to keep the test reasonably fast.
        let probe_limit = 8_usize.min(probe_models.len());

        // 3) Probe each selected model/deployment (best-effort) and print response snippet
        let mut any_success = false;
        for model_name in probe_models.into_iter().take(probe_limit) {
            eprintln!("Probing model/deployment: {}", model_name);
            // For Azure OpenAI-style deployments the deployment name is often the same as the model id;
            // pass it as the deployment_name to probe_use_model so the helper will attempt deployment-style URIs.
            let deployment_name = Some(model_name.clone());

            // Try a short prompt
            let result = block_on(crate::provider::azure_foundry::probe_use_model(
                http_client.as_ref(),
                &endpoint,
                &api_key,
                &model_name,
                deployment_name,
                None,
                "Hello from Zed integration test",
            ));

            match result {
                Ok(body) => {
                    any_success = true;
                    let snippet: String = body.chars().take(1000).collect();
                    eprintln!(
                        "Probe success for model {}: response len {} snippet:\n{}\n---",
                        model_name,
                        body.len(),
                        snippet
                    );
                }
                Err(err) => {
                    eprintln!("Probe failed for model {}: {}", model_name, err);
                }
            }
        }

        // Require that at least one probe succeeded to consider the integration smoke test successful.
        assert!(any_success, "No successful probes for specified models");
    }
}
