// h3 wrappings for msquic

use std::{
    fmt::Display,
    future::Future,
    pin::{self, pin},
    sync::Arc,
    task::Poll,
};

use bytes::{Buf, BytesMut};
use c2::SEND_FLAG_NONE;
use h3::quic::{BidiStream, Connection, OpenStreams, RecvStream, SendStream, StreamId};

use crate::{conn::QConnection, stream::QStream};

#[derive(Debug)]
pub struct H3Error {
    status: std::io::Error,
    error_code: Option<u64>,
}

impl H3Error{
    pub fn new(status: std::io::Error, ec: Option<u64>) -> Self{
        Self { status, error_code: ec }
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

pub struct H3Conn {
    inner: QConnection,
}

impl<B: Buf> OpenStreams<B> for H3Conn {
    type BidiStream = H3Stream;

    type SendStream = H3Stream;

    type RecvStream = H3Stream;

    type Error = H3Error;

    fn poll_open_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, Self::Error>> {
        todo!()
    }

    fn poll_open_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
        todo!()
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        todo!()
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
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::RecvStream>, Self::Error>> {
        todo!()
    }

    fn poll_accept_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::BidiStream>, Self::Error>> {
        todo!()
    }

    fn poll_open_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, Self::Error>> {
        todo!()
    }

    fn poll_open_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, Self::Error>> {
        todo!()
    }

    fn opener(&self) -> Self::OpenStreams {
        todo!()
    }

    fn close(&mut self, code: h3::error::Code, reason: &[u8]) {
        todo!()
    }
}

pub struct H3Stream {
    inner: QStream,
    id: h3::quic::StreamId,
    //read:
}

impl H3Stream {
    fn new(s: QStream, id: StreamId) -> Self {
        Self { inner: s, id }
    }
}

impl<B: Buf> SendStream<B> for H3Stream {
    type Error = H3Error;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // always ready to send?
        // convert this to open or start?
        // if send is in progress?
        Poll::Ready(Ok(()))
    }

    fn send_data<T: Into<h3::quic::WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        let b: h3::quic::WriteBuf<B> = data.into();
        self.inner.send_only(b, SEND_FLAG_NONE);
        Ok(())
    }

    fn poll_finish(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_send(cx).map_err(|e|{H3Error::new(e, None)})
    }

    fn reset(&mut self, _reset_code: u64) {
        panic!("reset not supported")
    }

    fn send_id(&self) -> h3::quic::StreamId {
        self.id
    }
}

impl RecvStream for H3Stream {
    type Buf = BytesMut;

    type Error = H3Error;

    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::Buf>, Self::Error>> {
        //        let fu = self.inner.receive();
        // let innner = <Receiver<T> as Future>::poll(Pin::new(&mut self.rx), _cx);
        //Pin::new(&mut fu).poll(cx);
        // let mut pinned_fut = pin!(fu);
        // pinned_fut.poll(cx);
        todo!()
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.inner.stop_sending(error_code);
    }

    fn recv_id(&self) -> h3::quic::StreamId {
        self.id
    }
}

impl<B: Buf> BidiStream<B> for H3Stream {
    type SendStream = H3Stream;

    type RecvStream = H3Stream;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let cp = self.inner.clone();
        let id = self.id.clone();
        (self, H3Stream::new(cp, id))
    }
}
