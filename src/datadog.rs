use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context as _};

const DEFAULT_API_URL: &str = "https://api.datadoghq.com";
const DATADOG_ROLE_NAME: &str = "DatadogAWSIntegrationRole";
const EXPECTED_NAMESPACE_EXCLUSIONS: [&str; 4] =
    ["AWS/ElasticMapReduce", "AWS/Lambda", "AWS/SQS", "AWS/Usage"];

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
#[serde(deny_unknown_fields)]
struct AwsAccountAttributes {
    aws_account_id: String,
    #[serde(default)]
    account_tags: Vec<String>,
    #[serde(default)]
    auth_config: Option<AwsAuthConfig>,
    #[serde(default)]
    aws_partition: Option<String>,
    #[serde(default)]
    aws_regions: Option<AwsRegions>,
    #[serde(default, rename = "created_at")]
    _created_at: Option<serde_json::Value>,
    #[serde(default)]
    logs_config: Option<LogsConfig>,
    #[serde(default)]
    metrics_config: Option<MetricsConfig>,
    #[serde(default, rename = "modified_at")]
    _modified_at: Option<serde_json::Value>,
    #[serde(default)]
    resources_config: Option<ResourcesConfig>,
    #[serde(default)]
    traces_config: Option<TracesConfig>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AwsAuthConfig {
    #[serde(default)]
    role_name: Option<String>,
    #[serde(default, rename = "external_id")]
    _external_id: Option<serde_json::Value>,
    #[serde(default)]
    access_key_id: Option<String>,
    #[serde(default, rename = "secret_access_key")]
    _secret_access_key: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AwsRegions {
    #[serde(default)]
    include_all: Option<bool>,
    #[serde(default)]
    include_only: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogsConfig {
    #[serde(default)]
    lambda_forwarder: Option<LambdaForwarder>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LambdaForwarder {
    #[serde(default)]
    lambdas: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    log_source_config: Option<LogSourceConfig>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogSourceConfig {
    #[serde(default)]
    tag_filters: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MetricsConfig {
    #[serde(default)]
    automute_enabled: Option<bool>,
    #[serde(default)]
    collect_cloudwatch_alarms: Option<bool>,
    #[serde(default)]
    collect_custom_metrics: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    metric_name_filters: Vec<serde_json::Value>,
    #[serde(default)]
    namespace_filters: Option<NamespaceFilters>,
    #[serde(default)]
    tag_filters: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceFilters {
    #[serde(default)]
    exclude_only: Option<Vec<String>>,
    #[serde(default)]
    include_only: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcesConfig {
    #[serde(default)]
    cloud_security_posture_management_collection: Option<bool>,
    #[serde(default)]
    extended_collection: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TracesConfig {
    #[serde(default)]
    xray_services: Option<XrayServices>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct XrayServices {
    #[serde(default)]
    include_all: Option<bool>,
    #[serde(default)]
    include_only: Option<Vec<String>>,
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

    println!("Auditing live Datadog AWS integration configurations");
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
    let conflicts = response
        .data
        .iter()
        .filter_map(|account| {
            let conflicts = migration_conflicts(&account.attributes);
            (!conflicts.is_empty())
                .then_some((account.attributes.aws_account_id.as_str(), conflicts))
        })
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        let details = conflicts
            .into_iter()
            .flat_map(|(account_id, conflicts)| {
                std::iter::once(format!("  AWS account {account_id}:")).chain(
                    conflicts
                        .into_iter()
                        .map(|conflict| format!("    - {conflict}")),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "live Datadog AWS settings are not represented by the migration Terraform configuration:\n{details}\n\
             encode these settings in Terraform before running the migration"
        );
    }

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

fn migration_conflicts(attributes: &AwsAccountAttributes) -> Vec<String> {
    let mut conflicts = Vec::new();

    if attributes.account_tags.len() != 1
        || !matches!(
            attributes.account_tags.first().map(String::as_str),
            Some("env:prod" | "env:staging")
        )
    {
        conflicts.push(format!(
            "account_tags is {:?}; expected exactly env:prod or env:staging",
            attributes.account_tags
        ));
    }

    match &attributes.auth_config {
        Some(auth_config) => {
            if auth_config.access_key_id.is_some() {
                conflicts.push(
                    "auth_config uses access keys; the Terraform configuration declares an IAM role"
                        .to_string(),
                );
            }
            if auth_config.role_name.as_deref() != Some(DATADOG_ROLE_NAME) {
                conflicts.push(format!(
                    "auth_config.role_name is {:?}; expected {DATADOG_ROLE_NAME}",
                    auth_config.role_name
                ));
            }
        }
        None => conflicts.push("auth_config is missing".to_string()),
    }

    if attributes.aws_partition.as_deref().unwrap_or("aws") != "aws" {
        conflicts.push(format!(
            "aws_partition is {:?}; expected aws",
            attributes.aws_partition
        ));
    }

    if let Some(aws_regions) = &attributes.aws_regions {
        if let Some(include_only) = &aws_regions.include_only {
            conflicts.push(format!(
                "aws_regions.include_only is {include_only:?}; Terraform enables all regions"
            ));
        } else if !aws_regions.include_all.unwrap_or(true) {
            conflicts.push(
                "aws_regions.include_all is false; Terraform enables all regions".to_string(),
            );
        }
    }

    if let Some(lambda_forwarder) = attributes
        .logs_config
        .as_ref()
        .and_then(|config| config.lambda_forwarder.as_ref())
    {
        if !lambda_forwarder.lambdas.is_empty() {
            conflicts.push(format!(
                "logs_config.lambda_forwarder.lambdas is {:?}; Terraform declares no forwarders",
                lambda_forwarder.lambdas
            ));
        }
        if !lambda_forwarder.sources.is_empty() {
            conflicts.push(format!(
                "logs_config.lambda_forwarder.sources is {:?}; Terraform declares no log sources",
                lambda_forwarder.sources
            ));
        }
        if let Some(tag_filters) = lambda_forwarder
            .log_source_config
            .as_ref()
            .map(|config| &config.tag_filters)
            .filter(|tag_filters| !tag_filters.is_empty())
        {
            conflicts.push(format!(
                "logs_config.lambda_forwarder.log_source_config.tag_filters is {tag_filters:?}; Terraform declares no tag filters"
            ));
        }
    }

    audit_metrics_config(attributes.metrics_config.as_ref(), &mut conflicts);
    audit_resources_config(attributes.resources_config.as_ref(), &mut conflicts);
    audit_traces_config(attributes.traces_config.as_ref(), &mut conflicts);
    conflicts
}

fn audit_metrics_config(config: Option<&MetricsConfig>, conflicts: &mut Vec<String>) {
    let automute_enabled = config
        .and_then(|config| config.automute_enabled)
        .unwrap_or(true);
    if !automute_enabled {
        conflicts.push("metrics_config.automute_enabled is false; Terraform sets true".to_string());
    }
    let collect_cloudwatch_alarms = config
        .and_then(|config| config.collect_cloudwatch_alarms)
        .unwrap_or(false);
    if collect_cloudwatch_alarms {
        conflicts.push(
            "metrics_config.collect_cloudwatch_alarms is true; Terraform sets false".to_string(),
        );
    }
    let collect_custom_metrics = config
        .and_then(|config| config.collect_custom_metrics)
        .unwrap_or(false);
    if collect_custom_metrics {
        conflicts.push(
            "metrics_config.collect_custom_metrics is true; Terraform sets false".to_string(),
        );
    }
    let enabled = config.and_then(|config| config.enabled).unwrap_or(true);
    if !enabled {
        conflicts.push("metrics_config.enabled is false; Terraform sets true".to_string());
    }

    if let Some(metric_name_filters) = config
        .map(|config| &config.metric_name_filters)
        .filter(|filters| !filters.is_empty())
    {
        conflicts.push(format!(
            "metrics_config.metric_name_filters is {metric_name_filters:?}; Terraform declares no metric-name filters"
        ));
    }
    if let Some(tag_filters) = config
        .map(|config| &config.tag_filters)
        .filter(|filters| !filters.is_empty())
    {
        conflicts.push(format!(
            "metrics_config.tag_filters is {tag_filters:?}; Terraform declares no metric tag filters"
        ));
    }

    let default_exclusions = ["AWS/ElasticMapReduce", "AWS/SQS", "AWS/Usage"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let namespace_filters = config.and_then(|config| config.namespace_filters.as_ref());
    if let Some(include_only) = namespace_filters.and_then(|filters| filters.include_only.as_ref())
    {
        conflicts.push(format!(
            "metrics_config.namespace_filters.include_only is {include_only:?}; Terraform uses exclude_only"
        ));
    } else {
        let exclusions = namespace_filters
            .and_then(|filters| filters.exclude_only.as_ref())
            .unwrap_or(&default_exclusions);
        let actual = exclusions
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let expected = EXPECTED_NAMESPACE_EXCLUSIONS
            .into_iter()
            .collect::<BTreeSet<_>>();
        if exclusions.len() != EXPECTED_NAMESPACE_EXCLUSIONS.len() || actual != expected {
            conflicts.push(format!(
                "metrics_config.namespace_filters.exclude_only is {exclusions:?}; expected {EXPECTED_NAMESPACE_EXCLUSIONS:?}"
            ));
        }
    }
}

fn audit_resources_config(config: Option<&ResourcesConfig>, conflicts: &mut Vec<String>) {
    let cspm_enabled = config
        .and_then(|config| config.cloud_security_posture_management_collection)
        .unwrap_or(false);
    if cspm_enabled {
        conflicts.push(
            "resources_config.cloud_security_posture_management_collection is true; Terraform sets false"
                .to_string(),
        );
    }
    let extended_collection = config
        .and_then(|config| config.extended_collection)
        .unwrap_or(true);
    if extended_collection {
        conflicts
            .push("resources_config.extended_collection is true; Terraform sets false".to_string());
    }
}

fn audit_traces_config(config: Option<&TracesConfig>, conflicts: &mut Vec<String>) {
    if let Some(xray_services) = config.and_then(|config| config.xray_services.as_ref()) {
        if xray_services.include_all.unwrap_or(false) {
            conflicts.push(
                "traces_config.xray_services.include_all is true; Terraform disables X-Ray"
                    .to_string(),
            );
        }
        if xray_services
            .include_only
            .as_ref()
            .is_some_and(|services| !services.is_empty())
        {
            conflicts.push(format!(
                "traces_config.xray_services.include_only is {:?}; Terraform disables X-Ray",
                xray_services.include_only
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compatible_account(id: &str, aws_account_id: &str) -> AwsAccount {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "attributes": {
                "aws_account_id": aws_account_id,
                "account_tags": ["env:prod"],
                "auth_config": {
                    "role_name": DATADOG_ROLE_NAME,
                    "external_id": "generated-by-datadog"
                },
                "aws_partition": "aws",
                "aws_regions": { "include_all": true },
                "logs_config": {
                    "lambda_forwarder": {
                        "lambdas": [],
                        "sources": []
                    }
                },
                "metrics_config": {
                    "automute_enabled": true,
                    "collect_cloudwatch_alarms": false,
                    "collect_custom_metrics": false,
                    "enabled": true,
                    "metric_name_filters": [],
                    "namespace_filters": {
                        "exclude_only": EXPECTED_NAMESPACE_EXCLUSIONS
                    },
                    "tag_filters": []
                },
                "resources_config": {
                    "cloud_security_posture_management_collection": false,
                    "extended_collection": false
                },
                "traces_config": {
                    "xray_services": { "include_only": [] }
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn indexes_config_ids_by_aws_account_id() {
        let response = AwsAccountsResponse {
            data: vec![compatible_account("config-id", "012345678901")],
        };

        let config_ids = config_ids_from_response(response).unwrap();
        assert_eq!(config_ids.get("012345678901"), Some("config-id"));
        assert_eq!(config_ids.get("999999999999"), None);
    }

    #[test]
    fn rejects_duplicate_configs_for_one_aws_account() {
        let response = AwsAccountsResponse {
            data: vec![
                compatible_account("first", "012345678901"),
                compatible_account("second", "012345678901"),
            ],
        };

        let error = config_ids_from_response(response).unwrap_err();
        assert!(error.to_string().contains("multiple integration configs"));
    }

    #[test]
    fn rejects_live_settings_not_represented_in_terraform() {
        let account = serde_json::from_value::<AwsAccount>(serde_json::json!({
            "id": "config-id",
            "attributes": {
                "aws_account_id": "012345678901",
                "account_tags": ["env:prod", "team:infra"],
                "auth_config": {
                    "role_name": DATADOG_ROLE_NAME,
                    "external_id": "generated-by-datadog"
                },
                "aws_partition": "aws",
                "aws_regions": { "include_only": ["eu-west-1"] },
                "logs_config": {
                    "lambda_forwarder": {
                        "lambdas": ["arn:aws:lambda:eu-west-1:012345678901:function:forwarder"],
                        "sources": ["s3"]
                    }
                },
                "metrics_config": {
                    "automute_enabled": false,
                    "collect_cloudwatch_alarms": true,
                    "collect_custom_metrics": true,
                    "enabled": false,
                    "metric_name_filters": [{ "namespace": "AWS/EC2" }],
                    "namespace_filters": { "include_only": ["AWS/EC2"] },
                    "tag_filters": [{ "namespace": "AWS/EC2", "tags": ["team:infra"] }]
                },
                "resources_config": {
                    "cloud_security_posture_management_collection": true,
                    "extended_collection": true
                },
                "traces_config": {
                    "xray_services": { "include_only": ["api"] }
                }
            }
        }))
        .unwrap();

        let error = config_ids_from_response(AwsAccountsResponse {
            data: vec![account],
        })
        .unwrap_err()
        .to_string();
        for expected in [
            "account_tags",
            "aws_regions.include_only",
            "logs_config.lambda_forwarder.lambdas",
            "metrics_config.automute_enabled",
            "metrics_config.collect_cloudwatch_alarms",
            "metrics_config.collect_custom_metrics",
            "metrics_config.enabled",
            "metrics_config.metric_name_filters",
            "metrics_config.namespace_filters.include_only",
            "metrics_config.tag_filters",
            "resources_config.cloud_security_posture_management_collection",
            "resources_config.extended_collection",
            "traces_config.xray_services.include_only",
        ] {
            assert!(error.contains(expected), "missing {expected} in {error}");
        }
    }
}
