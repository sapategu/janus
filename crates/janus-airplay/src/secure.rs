//! HAP channel framing (Doubletake 5e2f1cb `client.go` encrypt /
//! readEncryptedFrame): `u16 LE length ‖ ChaCha20-Poly1305(chunk ≤ 1024) ‖ tag`,
//! the length prefix as AAD and a 64-bit little-endian counter nonce per
//! direction. Plaintext passes through until keys are installed; a failed
//! frame poisons the channel.
use crate::{
    Error,
    hap::{Aead, ChannelKeys, counter},
};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, ready},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const FRAME: usize = 1024;
const TAG: usize = 16;

struct Keys {
    read: Aead,
    write: Aead,
    read_nonce: u64,
    write_nonce: u64,
}
pub(crate) struct Secure<S> {
    inner: S,
    keys: Option<Keys>,
    sealed: Vec<u8>,
    plain: Vec<u8>,
    plain_at: usize,
    out: Vec<u8>,
    out_at: usize,
    poisoned: bool,
}
impl<S> Secure<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self {
            inner,
            keys: None,
            sealed: Vec::new(),
            plain: Vec::new(),
            plain_at: 0,
            out: Vec::new(),
            out_at: 0,
            poisoned: false,
        }
    }
    /// Switches to encrypted framing. Bytes already buffered in either
    /// direction would straddle the boundary, so that is refused.
    pub(crate) fn enable(&mut self, keys: ChannelKeys) -> Result<(), Error> {
        if self.keys.is_some() || !self.sealed.is_empty() || self.out_at < self.out.len() {
            return Err(Error::Protocol);
        }
        self.keys = Some(Keys {
            read: Aead::new(&keys.read),
            write: Aead::new(&keys.write),
            read_nonce: 0,
            write_nonce: 0,
        });
        Ok(())
    }
}
fn invalid(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}
fn flush_out<S: AsyncWrite + Unpin>(
    inner: &mut S,
    out: &mut Vec<u8>,
    at: &mut usize,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    while *at < out.len() {
        let n = ready!(Pin::new(&mut *inner).poll_write(cx, &out[*at..]))?;
        if n == 0 {
            return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
        }
        *at += n;
    }
    out.clear();
    *at = 0;
    Poll::Ready(Ok(()))
}
impl<S: AsyncRead + Unpin> AsyncRead for Secure<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let Some(keys) = &mut this.keys else {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        };
        if this.poisoned {
            return Poll::Ready(Err(invalid("encrypted channel failed")));
        }
        loop {
            if this.plain_at < this.plain.len() {
                let n = buf.remaining().min(this.plain.len() - this.plain_at);
                buf.put_slice(&this.plain[this.plain_at..this.plain_at + n]);
                this.plain_at += n;
                return Poll::Ready(Ok(()));
            }
            if this.sealed.len() >= 2 {
                let len = usize::from(u16::from_le_bytes([this.sealed[0], this.sealed[1]]));
                if len == 0 || len > FRAME {
                    this.poisoned = true;
                    return Poll::Ready(Err(invalid("invalid encrypted frame length")));
                }
                if this.sealed.len() >= 2 + len + TAG {
                    let mut data = this.sealed[2..2 + len + TAG].to_vec();
                    let aad = [this.sealed[0], this.sealed[1]];
                    if keys
                        .read
                        .open(counter(keys.read_nonce), &aad, &mut data)
                        .is_err()
                    {
                        this.poisoned = true;
                        return Poll::Ready(Err(invalid("encrypted frame failed authentication")));
                    }
                    keys.read_nonce += 1;
                    this.sealed.drain(..2 + len + TAG);
                    this.plain = data;
                    this.plain_at = 0;
                    continue;
                }
            }
            let mut chunk = [0; 2048];
            let mut read = ReadBuf::new(&mut chunk);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut read))?;
            if read.filled().is_empty() {
                return Poll::Ready(if this.sealed.is_empty() {
                    Ok(())
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                });
            }
            this.sealed.extend_from_slice(read.filled());
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Secure<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let Some(keys) = &mut this.keys else {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        };
        ready!(flush_out(
            &mut this.inner,
            &mut this.out,
            &mut this.out_at,
            cx
        ))?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let chunk = &buf[..buf.len().min(FRAME)];
        let aad = (chunk.len() as u16).to_le_bytes();
        let mut data = chunk.to_vec();
        keys.write.seal(counter(keys.write_nonce), &aad, &mut data);
        keys.write_nonce += 1;
        this.out.extend_from_slice(&aad);
        this.out.extend(data);
        // The sealed chunk is owned now; delivery completes on the next
        // write, flush or shutdown.
        if let Poll::Ready(Err(e)) = flush_out(&mut this.inner, &mut this.out, &mut this.out_at, cx)
        {
            return Poll::Ready(Err(e));
        }
        Poll::Ready(Ok(chunk.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(flush_out(
            &mut this.inner,
            &mut this.out,
            &mut this.out_at,
            cx
        ))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(flush_out(
            &mut this.inner,
            &mut this.out,
            &mut this.out_at,
            cx
        ))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}
