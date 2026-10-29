mod artifacts;
mod bundle;
mod export;
mod imports;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{config::Config, model::Target};

pub use artifacts::{Artifact, Workspace, stage_input};
pub use bundle::stage_cooperative_manifest;
pub use export::{
    FrozenExport, FrozenPlan, PublishedExport, VerifiedExport, freeze_export, frozen_plan,
    prepare_export_chunk, publish_export, render_export, verify_export,
};
pub use imports::{download_reference, execute_import};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSpec {
    Export {
        config: Config,
    },
    WiaaDirectory {
        config: Config,
    },
    SchoolDirectory {
        csv: Artifact,
        manifest: Artifact,
    },
    PublicSchools {
        csv: Artifact,
        origin: Option<String>,
    },
    DownloadPublicSchools {
        expected_sha256: String,
    },
    PrivateSchools {
        csv: Artifact,
    },
    WiaaCooperatives {
        manifest: Artifact,
    },
    ReviewedIdentity {
        manifest: Artifact,
    },
    OfflineReplay {
        config: Config,
        target: Box<Target>,
        capture: Artifact,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub operation_id: String,
    pub spec: OperationSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationResult {
    pub operation_id: String,
    pub binding: String,
    pub receipt: Artifact,
    pub output: Option<PathBuf>,
}
