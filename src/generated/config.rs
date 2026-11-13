use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(alias = "a2aPath")]
    pub a_2_a_path: Option<String>,
    #[serde(alias = "a2aProtocolVersion")]
    pub a_2_a_protocol_version: String,
    #[serde(alias = "elicitationTimeoutSeconds")]
    pub elicitation_timeout_seconds: Option<i64>,
    #[serde(alias = "enableElicitation")]
    pub enable_elicitation: Option<bool>,
    #[serde(alias = "includeStructuredContent")]
    pub include_structured_content: Option<bool>,
    #[serde(alias = "maxElicitationRounds")]
    pub max_elicitation_rounds: Option<i64>,
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
    abi.setup()?;
    Ok(())
}
