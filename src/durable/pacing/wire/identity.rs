use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, ensure};
use sha2::{Digest, Sha256};
use url::Url;

use crate::model::Source;

pub struct WireRequest {
    pub(super) source: Source,
    pub(super) address: String,
    pub(super) origin: String,
    pub(super) method: String,
    pub(super) request_sha256: String,
}

impl WireRequest {
    pub fn new(source: Source, address: &str, method: &str, body: &[u8]) -> Result<Self> {
        ensure!(
            address.len() <= 8192 && body.len() <= crate::transport::MAX_BODY_BYTES,
            "wire identity exceeds bounds"
        );
        ensure!(
            matches!(method, "GET" | "POST" | "HEAD" | "OPTIONS"),
            "unsupported durable wire method"
        );
        let url = Url::parse(address).context("parsing durable wire origin")?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none(),
            "durable wire requires an HTTP origin without credentials"
        );
        let origin = url.origin().ascii_serialization();
        ensure!(origin.len() <= 2048, "durable wire origin exceeds bound");
        let body_sha256 = digest(body);
        let request_sha256 = digest(&serde_json::to_vec(&(
            source,
            address,
            method,
            body_sha256,
        ))?);
        Ok(Self {
            source,
            address: address.to_owned(),
            origin,
            method: method.to_owned(),
            request_sha256,
        })
    }
}

#[derive(Clone)]
pub(in crate::durable::pacing) struct WireContext {
    pub operation_id: String,
    pub attempt_id: String,
    counters: Arc<Mutex<Counters>>,
}

#[derive(Default)]
struct Counters {
    total: u16,
    occurrences: BTreeMap<String, u16>,
}

pub(in crate::durable::pacing) struct WireReceipt {
    pub wire_id: String,
    pub operation_id: String,
    pub attempt_id: String,
    pub request_sha256: String,
    pub occurrence: i32,
}

impl WireContext {
    pub fn new(operation_id: &str, attempt_id: &str) -> Result<Self> {
        ensure!(
            [operation_id, attempt_id].into_iter().all(valid_digest),
            "invalid durable wire operation or attempt SHA-256"
        );
        Ok(Self {
            operation_id: operation_id.to_owned(),
            attempt_id: attempt_id.to_owned(),
            counters: Arc::new(Mutex::new(Counters::default())),
        })
    }

    pub fn next(&self, request: &WireRequest) -> Result<WireReceipt> {
        let mut counters = self
            .counters
            .lock()
            .map_err(|_| anyhow!("wire identity counters poisoned"))?;
        ensure!(counters.total < 512, "durable wire request budget exceeded");
        counters.total = counters.total.saturating_add(1);
        let count = counters
            .occurrences
            .entry(request.request_sha256.clone())
            .or_insert(0);
        *count = count.saturating_add(1);
        let occurrence = i32::from(*count);
        drop(counters);
        let wire_id = digest(&serde_json::to_vec(&(
            &self.operation_id,
            &self.attempt_id,
            &request.request_sha256,
            occurrence,
        ))?);
        Ok(WireReceipt {
            wire_id,
            operation_id: self.operation_id.clone(),
            attempt_id: self.attempt_id.clone(),
            request_sha256: request.request_sha256.clone(),
            occurrence,
        })
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
