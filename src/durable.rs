mod admission;
pub mod blocking;
pub(crate) mod capacity_proof;
pub mod cli;
mod delivery;
mod effects;
pub mod model;
pub mod operations;
pub mod pacing;
pub mod page;
mod runtime;
mod services;
mod wiaa;

pub(crate) use effects::transient as retryable_error;
pub use runtime::serve;
pub use services::{AthleticCrawl, AthleticOperations, AthleticPage};

#[derive(Debug)]
pub(crate) struct AcquisitionBusy;

impl std::fmt::Display for AcquisitionBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("another scraper owns the acquisition session")
    }
}

impl std::error::Error for AcquisitionBusy {}
