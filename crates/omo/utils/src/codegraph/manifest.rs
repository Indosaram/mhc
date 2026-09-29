use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const CODEGRAPH_PINNED_VERSION: &str = "1.5.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodegraphProvisionAsset {
    pub executable_name: String,
    pub sha256: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodegraphProvisionManifest {
    pub assets: BTreeMap<String, CodegraphProvisionAsset>,
    pub version: String,
}

pub fn codegraph_provision_manifest() -> CodegraphProvisionManifest {
    let mut assets = BTreeMap::new();
    assets.insert(
        "darwin-arm64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: "cf5ee435a6e44d097b2f98f2b7b8b9422bb1094844404efed82519c5da1af2cf".to_string(),
            url: "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-darwin-arm64.tar.gz".to_string(),
        },
    );
    assets.insert(
        "darwin-x64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: "0a0ccc29bf7da9d10be1458d89d7e15c55927ae24cd95e9fa3de4bdfea059dde".to_string(),
            url: "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-darwin-x64.tar.gz".to_string(),
        },
    );
    assets.insert(
        "linux-arm64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: "9f17750aedf45d51f68caae39ed21d6e2a7290b2326e5c53f95a165918ebd1d8".to_string(),
            url: "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-linux-arm64.tar.gz".to_string(),
        },
    );
    assets.insert(
        "linux-x64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph".to_string(),
            sha256: "2ba65e87a1210b706bb1e67d5e48b5fc4a1935e43dbb3fb5f31c5597840d2e58".to_string(),
            url: "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-linux-x64.tar.gz".to_string(),
        },
    );
    assets.insert(
        "win32-arm64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph.cmd".to_string(),
            sha256: "19e0237ea5d8928f705d60e339eb319e7ec37490a69585712933c1534f3c0bc2".to_string(),
            url: "https://registry.npmjs.org/@colbymchenry/codegraph-win32-arm64/-/codegraph-win32-arm64-1.5.0.tgz".to_string(),
        },
    );
    assets.insert(
        "win32-x64".to_string(),
        CodegraphProvisionAsset {
            executable_name: "codegraph.cmd".to_string(),
            sha256: "ef64c878acb129885c2d8306ddec6674af865810b4c0f6a9ba9fcd61e21ff9d8".to_string(),
            url: "https://registry.npmjs.org/@colbymchenry/codegraph-win32-x64/-/codegraph-win32-x64-1.5.0.tgz".to_string(),
        },
    );

    CodegraphProvisionManifest {
        assets,
        version: CODEGRAPH_PINNED_VERSION.to_string(),
    }
}
