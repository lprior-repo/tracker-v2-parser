#![forbid(unsafe_code)]
#![recursion_limit = "256"]

pub mod acquisition;
mod athlete_identity_import;
mod browser;
pub mod cli;
pub mod config;
pub mod database;
pub mod durable;
pub mod export;
pub mod marks;
pub mod model;
pub mod native_ops;
mod profile_evidence;
pub mod runner;
pub mod school_address_download;
pub mod school_directory;
pub mod scope;
pub mod sources;
pub mod transport;
pub mod wiaa_coop_import;
pub mod wiaa_enrichment;
