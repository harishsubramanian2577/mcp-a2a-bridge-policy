use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(alias = "a2aPath")]
    pub a_2_a_path: Option<String>,
    #[serde(alias = "a2aProtocolVersion")]
    pub a_2_a_protocol_version: String,
    #[serde(alias = "agentCardAuthorization")]
    pub agent_card_authorization: Option<String>,
    #[serde(alias = "agentCardMaxStaleSeconds")]
    pub agent_card_max_stale_seconds: Option<i64>,
    #[serde(alias = "agentCardRefreshSeconds")]
    pub agent_card_refresh_seconds: Option<i64>,
    #[serde(alias = "agentCardTimeoutSeconds")]
    pub agent_card_timeout_seconds: Option<i64>,
    #[serde(
        alias = "agentCardUrl",
        default,
        deserialize_with = "pdk::serde::deserialize_service_opt"
    )]
    pub agent_card_url: Option<pdk::hl::Service>,
    #[serde(alias = "elicitationTimeoutSeconds")]
    pub elicitation_timeout_seconds: Option<i64>,
    #[serde(alias = "enableElicitation")]
    pub enable_elicitation: Option<bool>,
    #[serde(alias = "includeStructuredContent")]
    pub include_structured_content: Option<bool>,
    #[serde(alias = "maxElicitationRounds")]
    pub max_elicitation_rounds: Option<i64>,
    #[serde(alias = "metadataMode")]
    pub metadata_mode: Option<String>,
    #[serde(alias = "passThroughA2A")]
    pub pass_through_a_2_a: Option<bool>,
    #[serde(alias = "serverInstructions")]
    pub server_instructions: Option<String>,
    #[serde(alias = "serverName")]
    pub server_name: Option<String>,
    #[serde(alias = "sessionKeyHeader")]
    pub session_key_header: Option<String>,
    #[serde(alias = "sessionStoreMode")]
    pub session_store_mode: Option<String>,
    #[serde(alias = "sessionTtlSeconds")]
    pub session_ttl_seconds: Option<i64>,
    #[serde(alias = "toolDescription")]
    pub tool_description: Option<String>,
    #[serde(alias = "toolName")]
    pub tool_name: String,
}
#[pdk::hl::entrypoint_flex]
fn init(abi: &dyn pdk::flex_abi::api::FlexAbi) -> Result<(), anyhow::Error> {
    let config: Config = serde_json::from_slice(abi.get_configuration())
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to parse configuration '{}'. Cause: {}",
                String::from_utf8_lossy(abi.get_configuration()), err
            )
        })?;
    if config.agent_card_url.is_some() {
        let service = config.agent_card_url.unwrap();
        abi.service_create(service)?;
    }
    abi.setup()?;
    Ok(())
}
