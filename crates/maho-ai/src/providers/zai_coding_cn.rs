//! Port of senpi packages/ai/src/providers/zai-coding-cn.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub fn zai_coding_cn_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "zai-coding-cn".into(),
        name: Some("Z.AI Coding CN".into()),
        base_url: Some("https://open.bigmodel.cn/api/coding/paas/v4".into()),
        headers: None,
        models: super::zai_coding_cn::zai_coding_cn_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("openai-completions")),
    })
}
