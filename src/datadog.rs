use std::collections::BTreeMap;

use anyhow::{bail, Context as _};

const DEFAULT_API_URL: &str = "https://api.datadoghq.com";

#[derive(Debug)]
pub struct AwsAccountConfigIds {
    by_aws_account_id: BTreeMap<String, String>,
}

impl AwsAccountConfigIds {
    pub fn get(&self, aws_account_id: &str) -> Option<&str> {
        self.by_aws_account_id
            .get(aws_account_id)
            .map(String::as_str)
    }
}

#[derive(serde::Deserialize)]
struct AwsAccountsResponse {
    data: Vec<AwsAccount>,
}

#[derive(serde::Deserialize)]
struct AwsAccount {
    id: String,
    attributes: AwsAccountAttributes,
}

#[derive(serde::Deserialize)]
struct AwsAccountAttributes {
    aws_account_id: String,
}

pub async fn load_aws_account_config_ids() -> anyhow::Result<AwsAccountConfigIds> {
    let api_key = std::env::var("DD_API_KEY")
        .context("DD_API_KEY must be set to migrate Datadog AWS integrations")?;
    let app_key = std::env::var("DD_APP_KEY")
        .context("DD_APP_KEY must be set to migrate Datadog AWS integrations")?;
    let api_url = std::env::var("DD_HOST").unwrap_or_else(|_| DEFAULT_API_URL.to_string());
    let endpoint = format!(
        "{}/api/v2/integration/aws/accounts",
        api_url.trim_end_matches('/')
    );

    println!("Fetching Datadog AWS integration config IDs");
    let response = reqwest::Client::new()
        .get(endpoint)
        .header("DD-API-KEY", api_key)
        .header("DD-APPLICATION-KEY", app_key)
        .send()
        .await
        .context("failed to list Datadog AWS integrations")?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("failed to list Datadog AWS integrations: HTTP {status}: {body}");
    }

    let response = response
        .json::<AwsAccountsResponse>()
        .await
        .context("Datadog returned an invalid AWS integrations response")?;
    config_ids_from_response(response)
}

fn config_ids_from_response(response: AwsAccountsResponse) -> anyhow::Result<AwsAccountConfigIds> {
    let mut by_aws_account_id = BTreeMap::new();
    for account in response.data {
        if let Some(existing) = by_aws_account_id.insert(
            account.attributes.aws_account_id.clone(),
            account.id.clone(),
        ) {
            bail!(
                "Datadog returned multiple integration configs for AWS account {}: {existing} and {}",
                account.attributes.aws_account_id,
                account.id
            );
        }
    }
    Ok(AwsAccountConfigIds { by_aws_account_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_config_ids_by_aws_account_id() {
        let response = serde_json::from_str::<AwsAccountsResponse>(
            r#"{
                "data": [
                    {
                        "id": "config-id",
                        "attributes": { "aws_account_id": "012345678901" }
                    }
                ]
            }"#,
        )
        .unwrap();

        let config_ids = config_ids_from_response(response).unwrap();
        assert_eq!(config_ids.get("012345678901"), Some("config-id"));
        assert_eq!(config_ids.get("999999999999"), None);
    }

    #[test]
    fn rejects_duplicate_configs_for_one_aws_account() {
        let response = serde_json::from_str::<AwsAccountsResponse>(
            r#"{
                "data": [
                    {
                        "id": "first",
                        "attributes": { "aws_account_id": "012345678901" }
                    },
                    {
                        "id": "second",
                        "attributes": { "aws_account_id": "012345678901" }
                    }
                ]
            }"#,
        )
        .unwrap();

        let error = config_ids_from_response(response).unwrap_err();
        assert!(error.to_string().contains("multiple integration configs"));
    }
}
