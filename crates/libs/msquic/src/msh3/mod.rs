// h3 wrappings for msquic

use std::{fmt::Display, task::Poll};

use bytes::{Buf, BytesMut};
use c2::{SEND_FLAG_NONE, STREAM_OPEN_FLAG_NONE, STREAM_OPEN_FLAG_UNIDIRECTIONAL};
use h3::quic::{BidiStream, Connection, OpenStreams, RecvStream, SendStream};

use crate::{conn::QConnection, stream::QStream};

#[derive(Debug)]
pub struct H3Error {
    status: std::io::Error,
    error_code: Option<u64>,
}

impl H3Error {
    pub fn new(status: std::io::Error, ec: Option<u64>) -> Self {
        Self {
            status,
            error_code: ec,
        }
    }
}

impl h3::quic::Error for H3Error {
    fn is_timeout(&self) -> bool {
        self.status.kind() == std::io::ErrorKind::TimedOut
    }

    fn err_code(&self) -> Option<u64> {
        self.error_code
    }
}

impl std::error::Error for H3Error {}

impl Display for H3Error {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

pub struct H3Conn {
    _inner: QConnection,
}

impl<B: Buf> OpenStreams<B> for H3Conn {
    type BidiStream = H3Stream;

    type SendStream = H3Stream;

    type RecvStream = H3Stream;

    type Error = H3Error;

    fn poll_open_bidi(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, Self::Error>> {
        let s = QStream::open(&self._inner, STREAM_OPEN_FLAG_NONE);
        // TODO: start?
        Poll::Ready(Ok(H3Stream::new(s)))
    }

    fn poll_open_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
        let s = QStream::open(&self._inner, STREAM_OPEN_FLAG_UNIDIRECTIONAL);
        // TODO: start?
        Poll::Ready(Ok(H3Stream::new(s)))
    }

    fn close(&mut self, code: h3::error::Code, _reason: &[u8]) {
        // TODO?
        self._inner.shutdown_only(code.value())
    }
}

impl<B: Buf> Connection<B> for H3Conn {
    type BidiStream = H3Stream;

    type SendStream = H3Stream;

    type RecvStream = H3Stream;

    type OpenStreams = H3Conn;

    type Error = H3Error;

    fn poll_accept_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::RecvStream>, Self::Error>> {
        todo!()
    }

    fn poll_accept_bidi(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::BidiStream>, Self::Error>> {
        todo!()
    }

    fn poll_open_bidi(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, Self::Error>> {
        todo!()
    }

    fn poll_open_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
        todo!()
    }

    fn opener(&self) -> Self::OpenStreams {
        todo!()
    }

    fn close(&mut self, code: h3::error::Code, _reason: &[u8]) {
        self._inner.shutdown_only(code.value())
    }
}

pub struct H3Stream {
    inner: QStream,
    shutdown: bool,
}

impl H3Stream {
    fn new(s: QStream) -> Self {
        Self {
            inner: s,
            shutdown: false,
        }
    }
}

impl<B: Buf> SendStream<B> for H3Stream {
    type Error = H3Error;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner
            .poll_ready_send(cx)
            .map_err(|e| H3Error::new(e, None))
    }

    fn send_data<T: Into<h3::quic::WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        let b: h3::quic::WriteBuf<B> = data.into();
        self.inner.send_only(b, SEND_FLAG_NONE);
        Ok(())
    }

    // send shutdown signal to peer.
    fn poll_finish(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // close the stream
        if !self.shutdown {
            self.inner.shutdown_only();
            self.shutdown = true;
        }
        self.inner
            .poll_shutdown(cx)
            .map_err(|e| H3Error::new(e, None))
    }

    fn reset(&mut self, _reset_code: u64) {
        panic!("reset not supported")
    }

    fn send_id(&self) -> h3::quic::StreamId {
        self.inner.get_id().try_into().expect("cannot convert id")
    }
}

impl RecvStream for H3Stream {
    type Buf = BytesMut;

    type Error = H3Error;

    // currently error is not propagated.
    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::Buf>, Self::Error>> {
        match self.inner.poll_receive(cx) {
            std::task::Poll::Ready(br) => Poll::Ready(Ok(br)),
            std::task::Poll::Pending => Poll::Pending,
        }
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.inner.stop_sending(error_code);
    }

    fn recv_id(&self) -> h3::quic::StreamId {
        let id = self.inner.get_id();
        id.try_into().expect("invalid stream id")
    }
}

impl<B: Buf> BidiStream<B> for H3Stream {
    type SendStream = H3Stream;

    type RecvStream = H3Stream;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let cp = self.inner.clone();
        (self, H3Stream::new(cp))
    }
}
