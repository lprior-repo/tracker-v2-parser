const MAX_TARGET_PREFIX: usize = 256;
const SCHEMES: [&[u8]; 2] = [b"http://", b"https://"];
const PATHS: [&[u8]; 3] = [b"/favicon.ico", b"/generate_204", b"/gen_204"];

#[derive(Clone, Copy)]
pub(super) enum Method {
    Get,
    Head,
    Options,
    Other,
}

#[derive(Clone, Copy)]
pub(super) enum Path {
    Favicon,
    Probe,
    Other,
}

pub(super) struct Prefix {
    word: [u8; 7],
    length: usize,
    method: Method,
    target: Option<Target>,
}

impl Prefix {
    pub fn new(first: u8) -> Option<Self> {
        matches!(first, b'G' | b'H' | b'O').then_some(Self {
            word: [first, 0, 0, 0, 0, 0, 0],
            length: 1,
            method: Method::Other,
            target: None,
        })
    }

    pub fn next(&mut self, byte: u8) -> Option<(Method, Path)> {
        if let Some(target) = self.target.as_mut() {
            return target.next(byte).map(|path| (self.method, path));
        }
        if byte == b' ' {
            self.method = match self.word.get(..self.length) {
                Some(b"GET") => Method::Get,
                Some(b"HEAD") => Method::Head,
                Some(b"OPTIONS") => Method::Options,
                _ => return Some((Method::Other, Path::Other)),
            };
            self.target = Some(Target::default());
            return None;
        }
        let expected: &[u8] = match self.word.first().copied() {
            Some(b'G') => b"GET",
            Some(b'H') => b"HEAD",
            Some(b'O') => b"OPTIONS",
            _ => return Some((Method::Other, Path::Other)),
        };
        if expected.get(self.length) != Some(&byte) {
            return Some((Method::Other, Path::Other));
        }
        let Some(slot) = self.word.get_mut(self.length) else {
            return Some((Method::Other, Path::Other));
        };
        *slot = byte;
        self.length = self.length.saturating_add(1);
        None
    }

    pub const fn ended(&self) -> (Method, Path) {
        (self.method, Path::Other)
    }
}

#[derive(Default)]
enum Stage {
    #[default]
    Start,
    Scheme,
    Authority,
    Path,
}

#[derive(Default)]
struct Target {
    stage: Stage,
    consumed: usize,
    index: usize,
    candidates: u8,
}

impl Target {
    fn next(&mut self, byte: u8) -> Option<Path> {
        self.consumed = self.consumed.saturating_add(1);
        if self.consumed > MAX_TARGET_PREFIX {
            return Some(Path::Other);
        }
        match self.stage {
            Stage::Start if byte == b'/' => {
                self.stage = Stage::Path;
                self.candidates = 7;
                self.path(byte)
            }
            Stage::Start if byte == b'h' => {
                self.stage = Stage::Scheme;
                self.candidates = 3;
                self.scheme(byte)
            }
            Stage::Start => Some(Path::Other),
            Stage::Scheme => self.scheme(byte),
            Stage::Authority if byte == b'/' => {
                self.stage = Stage::Path;
                self.index = 0;
                self.candidates = 7;
                self.path(byte)
            }
            Stage::Authority if byte.is_ascii_graphic() && !matches!(byte, b'?' | b'#') => None,
            Stage::Authority => Some(Path::Other),
            Stage::Path => self.path(byte),
        }
    }

    fn scheme(&mut self, byte: u8) -> Option<Path> {
        self.candidates = matching(self.candidates, self.index, byte, &SCHEMES);
        self.index = self.index.saturating_add(1);
        if self.candidates == 0 {
            return Some(Path::Other);
        }
        if SCHEMES.iter().enumerate().any(|(index, scheme)| {
            self.candidates & (1 << index) != 0 && scheme.len() == self.index
        }) {
            self.stage = Stage::Authority;
        }
        None
    }

    fn path(&mut self, byte: u8) -> Option<Path> {
        if matches!(byte, b' ' | b'?' | b'#' | b'\r' | b'\n') {
            return Some(match PATHS.iter().enumerate().find(|(index, path)| {
                self.candidates & (1 << index) != 0 && path.len() == self.index
            }).map(|(index, _)| index) {
                Some(0) => Path::Favicon,
                Some(1 | 2) => Path::Probe,
                _ => Path::Other,
            });
        }
        self.candidates = matching(self.candidates, self.index, byte, &PATHS);
        self.index = self.index.saturating_add(1);
        (self.candidates == 0).then_some(Path::Other)
    }
}

fn matching(candidates: u8, index: usize, byte: u8, patterns: &[&[u8]]) -> u8 {
    patterns.iter().enumerate().fold(0, |matches, (slot, pattern)| {
        if candidates & (1 << slot) != 0 && pattern.get(index) == Some(&byte) {
            matches | (1 << slot)
        } else {
            matches
        }
    })
}

#[cfg(test)]
mod review_tests {
    use anyhow::{Context, Result};

    use super::{Method, Path, Prefix};

    #[test]
    fn given_favicon_uri_with_secrets_when_classifying_then_reading_stops_before_query_or_headers() -> Result<()> {
        for request in [
            b"GET /favicon.ico?secret=not-read HTTP/1.1\r\nAuthorization: not-read\r\n\r\n".as_slice(),
            b"GET http://example.test/favicon.ico?secret=not-read HTTP/1.1\r\nAuthorization: not-read\r\n\r\n".as_slice(),
            b"GET https://example.test/favicon.ico?secret=not-read HTTP/1.1\r\nAuthorization: not-read\r\n\r\n".as_slice(),
        ] {
            let mut prefix = Prefix::new(b'G').context("GET prefix missing")?;
            let (index, observed) = request.iter().enumerate().skip(1).find_map(|(index, byte)| {
                prefix.next(*byte).map(|observed| (index, observed))
            }).context("HTTP metadata never completed")?;
            assert!(matches!(observed, (Method::Get, Path::Favicon)));
            assert_eq!(request.get(index), Some(&b'?'));
        }
        Ok(())
    }

    #[test]
    fn given_head_or_options_probe_when_classifying_then_only_static_categories_are_reported() -> Result<()> {
        for (request, head) in [
            (b"HEAD http://example.test/generate_204 HTTP/1.1\r\n\r\n".as_slice(), true),
            (b"OPTIONS /gen_204 HTTP/1.1\r\n\r\n".as_slice(), false),
        ] {
            let first = *request.first().context("method missing")?;
            let mut prefix = Prefix::new(first).context("HTTP prefix missing")?;
            let observed = request.iter().skip(1).find_map(|byte| prefix.next(*byte)).context("HTTP metadata never completed")?;
            assert!(matches!(observed, (Method::Head, Path::Probe)) && head
                || matches!(observed, (Method::Options, Path::Probe)) && !head);
        }
        Ok(())
    }
}
