use std::io::{self, ErrorKind, Write};

use anyhow::Result;

use crate::{durable::page::artifacts::MAX_PARSED_BYTES, model::ParsedPage};

pub(super) enum EncodedPage {
    Json(Vec<u8>),
    TooLarge,
}

#[derive(Default)]
struct BoundedJson {
    bytes: Vec<u8>,
}

impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|length| *length <= MAX_PARSED_BYTES)
            .ok_or_else(|| io::Error::from(ErrorKind::FileTooLarge))?;
        if required > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(128)
                .max(required)
                .min(MAX_PARSED_BYTES);
            self.bytes
                .try_reserve_exact(capacity.saturating_sub(self.bytes.len()))
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn encode(page: &ParsedPage) -> Result<EncodedPage> {
    let mut output = BoundedJson::default();
    match serde_json::to_writer(&mut output, page) {
        Ok(()) => Ok(EncodedPage::Json(output.bytes)),
        Err(error) if error.io_error_kind() == Some(ErrorKind::FileTooLarge) => {
            Ok(EncodedPage::TooLarge)
        }
        Err(error) => Err(error.into()),
    }
}
