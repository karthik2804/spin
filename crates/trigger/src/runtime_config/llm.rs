use spin_llm::RemoteComputeOpts;
use url::Url;

pub(crate) async fn build_component(
    runtime_config: &crate::RuntimeConfig,
    use_gpu: bool,
) -> spin_llm::LlmComponent {
    match runtime_config.llm_compute() {
        LlmComputeOpts::Spin => {
            spin_llm::LlmComponent::new(
                runtime_config
                    .state_dir()
                    .unwrap_or_default()
                    .join("ai-models"),
                use_gpu, false, None
            )
            .await
        }
        LlmComputeOpts::Http(config) => {
            spin_llm::LlmComponent::new(
                runtime_config
                    .state_dir()
                    .unwrap_or_default()
                    .join("ai-models"),
                use_gpu, true, Some(RemoteComputeOpts {
                    remote_url: config.url.to_owned(),
                    auth_token: config.auth_token.to_owned(),
                })
            )
            .await
        },
    }

}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum LlmComputeOpts {
    Spin,
    Http(HttpComputeOpts),
}


#[derive(Debug, serde::Deserialize)]
pub struct HttpComputeOpts {
    url: Url,
    auth_token: String,
}