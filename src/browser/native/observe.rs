use std::{
    future::Future,
    io::Read,
    net::{Shutdown, TcpStream as StdStream},
    pin::Pin,
    task::{Context, Poll},
};

use anyhow::{Context as _, Result, anyhow};
use futures::task::AtomicWaker;
use tokio::{
    io::{AsyncRead, ReadBuf},
    net::TcpStream,
};

const CONNECT: &[u8] = b"CONNECT ";
const MAX_AUTHORITY: usize = 320;
const MAX_PREFIX: usize = MAX_AUTHORITY.saturating_add(9);

#[derive(Clone, Copy)]
pub(super) enum Denial {
    NonTunnel,
    InvalidAuthority,
    TruncatedPrefix,
}

pub(super) enum Observation {
    Probe,
    Request(Denial),
    Connect(String),
}

struct Prefix {
    method: usize,
    authority: [u8; MAX_AUTHORITY],
    length: usize,
}

impl Prefix {
    fn next(&mut self, byte: u8) -> Result<Option<Observation>> {
        if self.method < CONNECT.len() {
            let matches = CONNECT.get(self.method) == Some(&byte);
            self.method = self.method.saturating_add(1);
            return Ok((!matches).then_some(Observation::Request(Denial::NonTunnel)));
        }
        if byte == b' ' {
            if self.length == 0 {
                return Ok(Some(Observation::Request(Denial::InvalidAuthority)));
            }
            let bytes = self
                .authority
                .get(..self.length)
                .context("native authority length exceeded buffer")?;
            let authority = std::str::from_utf8(bytes).context("native authority was not ASCII")?;
            let mut copied = String::new();
            copied.try_reserve(authority.len())?;
            copied.push_str(authority);
            return Ok(Some(Observation::Connect(copied)));
        }
        if !byte.is_ascii_alphanumeric() && !matches!(byte, b'.' | b'-' | b':' | b'[' | b']') {
            return Ok(Some(Observation::Request(Denial::InvalidAuthority)));
        }
        let Some(slot) = self.authority.get_mut(self.length) else {
            return Ok(Some(Observation::Request(Denial::InvalidAuthority)));
        };
        *slot = byte;
        self.length = self.length.saturating_add(1);
        Ok(None)
    }

    const fn ended(&self) -> Observation {
        if self.method == 0 {
            Observation::Probe
        } else {
            Observation::Request(Denial::TruncatedPrefix)
        }
    }

    fn available(&mut self, socket: &mut StdStream) -> Result<Observation> {
        std::iter::repeat_with(|| {
            let mut byte = [0; 1];
            match socket.read(&mut byte) {
                Ok(0) => Some(Ok(self.ended())),
                Ok(_) => byte
                    .first()
                    .copied()
                    .context("native byte buffer was empty")
                    .and_then(|byte| self.next(byte))
                    .transpose(),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ConnectionReset
                    ) =>
                {
                    Some(Ok(self.ended()))
                }
                Err(error) => Some(Err(error.into())),
            }
        })
        .take(MAX_PREFIX)
        .find_map(std::convert::identity)
        .context("native routing prefix exceeded limit")?
    }
}

pub(super) struct PendingSocket {
    socket: Option<TcpStream>,
    prefix: Prefix,
    quiescent: bool,
    waker: AtomicWaker,
}

impl PendingSocket {
    pub const fn new(socket: TcpStream) -> Self {
        Self {
            socket: Some(socket),
            prefix: Prefix {
                method: 0,
                authority: [0; MAX_AUTHORITY],
                length: 0,
            },
            quiescent: false,
            waker: AtomicWaker::new(),
        }
    }

    pub fn quiesce(&mut self) {
        self.quiescent = true;
        self.waker.wake();
    }

    fn finish(&mut self, observation: Option<Observation>) -> Result<Observation> {
        let mut socket = self
            .socket
            .take()
            .context("native socket already classified")?
            .into_std()?;
        let result = match observation {
            Some(observation) => Ok(observation),
            None => self.prefix.available(&mut socket),
        };
        if let Err(error) = socket.shutdown(Shutdown::Both) {
            tracing::debug!(%error, "closing nonforwarded native transport");
        }
        drop(socket);
        result
    }
}

impl Future for PendingSocket {
    type Output = Result<Observation>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let state = self.get_mut();
        state.waker.register(context.waker());
        if state.quiescent {
            return Poll::Ready(state.finish(None));
        }
        let Some(socket) = state.socket.as_mut() else {
            return Poll::Ready(Err(anyhow!(
                "native socket was polled after classification"
            )));
        };
        let mut byte = [0; 1];
        let mut buffer = ReadBuf::new(&mut byte);
        match Pin::new(socket).poll_read(context, &mut buffer) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) if buffer.filled().is_empty() => {
                let observation = state.prefix.ended();
                Poll::Ready(state.finish(Some(observation)))
            }
            Poll::Ready(Ok(())) => match buffer
                .filled()
                .first()
                .copied()
                .context("native byte buffer was empty")
                .and_then(|byte| state.prefix.next(byte))
            {
                Ok(Some(observation)) => Poll::Ready(state.finish(Some(observation))),
                Ok(None) => {
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
                Err(error) => Poll::Ready(Err(error)),
            },
            Poll::Ready(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                let observation = state.prefix.ended();
                Poll::Ready(state.finish(Some(observation)))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error.into())),
        }
    }
}
