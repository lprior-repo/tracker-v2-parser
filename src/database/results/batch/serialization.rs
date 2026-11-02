use std::io::{self, Write};

use serde_json::Value;

use crate::{
    database::results::Normalized,
    model::{Performance, Source},
};

pub(super) fn bind_payload_bytes(
    source: Source,
    performance: &Performance,
    key: &str,
    url: &str,
    normalized: &Normalized,
) -> Option<usize> {
    [
        key.len(),
        performance.athlete_id.len(),
        performance.school_id.as_ref().map_or(0, String::len),
        source.as_str().len(),
        option_len(performance.result_id.as_deref()),
        performance.event.len(),
        performance.mark.len(),
        performance.sport.as_str().len(),
        option_len(performance.meet_id.as_deref()),
        option_len(performance.meet_name.as_deref()),
        option_len(performance.date.as_deref()),
        option_len(performance.timing.as_deref()),
        option_len(performance.course.as_deref()),
        url.len(),
        option_len(performance.grade.as_deref()),
        option_len(performance.gender.as_deref()),
        option_len(normalized.unit),
        option_len(normalized.specification.as_deref()),
        normalized.status.len(),
    ]
    .into_iter()
    .try_fold(256_usize, usize::checked_add)
}

fn option_len(value: Option<&str>) -> usize {
    value.map_or(0, str::len)
}

pub(super) fn serialize_raw(performance: &Performance, limit: usize) -> Option<(Value, usize)> {
    let mut writer = LimitedWriter { bytes: 0, limit };
    serde_json::to_writer(&mut writer, performance).ok()?;
    let value = serde_json::to_value(performance).ok()?;
    Some((value, writer.bytes))
}

struct LimitedWriter {
    bytes: usize,
    limit: usize,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("result JSON payload size overflow"))?;
        if required > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "result JSON payload exceeds batch byte limit",
            ));
        }
        self.bytes = required;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
