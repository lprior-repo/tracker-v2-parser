mod crawl;
mod operations;
mod page;

use std::sync::Arc;

use super::runtime::Runtime;

pub struct AthleticCrawl {
    pub(super) runtime: Arc<Runtime>,
}

pub struct AthleticPage {
    pub(super) runtime: Arc<Runtime>,
}

pub struct AthleticOperations {
    pub(super) runtime: Arc<Runtime>,
}

pub use operations::AthleticOperationsClient;
pub use page::{AthleticPageClient, PageExecution};
