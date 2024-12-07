// h3 wrappings for msquic

use std::{fmt::Display, sync::Arc, task::Poll};

use bytes::{Buf, BytesMut};
use h3::quic::{BidiStream, Connection, OpenStreams, RecvStream, SendStream};
use msquic_sys2::{
    StreamOpenFlags, SEND_FLAG_NONE, STREAM_OPEN_FLAG_NONE, STREAM_OPEN_FLAG_UNIDIRECTIONAL,
    STREAM_SHUTDOWN_FLAG_GRACEFUL,
};

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

/// Quic connection
#[derive(Clone)]
pub struct H3Conn {
    inner: Arc<std::sync::Mutex<QConnection>>, // some functions require mut. we need clone. TODO: move mutex to inner.
    bidi: Arc<std::sync::Mutex<Option<H3Stream>>>, // holder for stream that is created but not started.
    send: Arc<std::sync::Mutex<Option<H3Stream>>>,
}

impl H3Conn {
    pub fn new(inner: QConnection) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(inner)),
            bidi: Default::default(),
            send: Default::default(),
        }
    }

    // poll a opened but not yet started stream.
    fn poll_start_stream(
        s: &mut QStream,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<H3Stream, H3Error>> {
        match s.poll_start(cx) {
            Poll::Ready(ok) => match ok {
                Ok(_) => Poll::Ready(Ok(H3Stream::new(s.clone()))),
                Err(e) => Poll::Ready(Err(H3Error::new(e, None))),
            },
            Poll::Pending => Poll::Pending,
        }
    }

    /// Because msquic open and start are separate steps, and h3 expects opened
    /// streams are started,
    /// we cache the opened stream in connection if it start is pending.
    /// the next poll from h3 will keep polling the cached stream for start.
    fn poll_open_inner(
        cache: &mut Arc<std::sync::Mutex<Option<H3Stream>>>,
        inner: &Arc<std::sync::Mutex<QConnection>>,
        flags: StreamOpenFlags,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<H3Stream, H3Error>> {
        // poll the cached stream waiting to be started.
        // take the lock longer to prevent race.
        let mut cache_lk = cache.lock().unwrap();
        if let Some(s) = cache_lk.as_mut() {
            let res = Self::poll_start_stream(&mut s.inner, cx);
            if res.is_ready() {
                // clear the cached bidi
                cache_lk.take();
            }
            res
        } else {
            let lk = inner.as_ref().lock().unwrap();
            let mut s = QStream::open(&lk, flags);
            s.start_only(flags);
            let res = Self::poll_start_stream(&mut s, cx);
            let s = H3Stream::new(s);
            if res.is_pending() {
                // cache it for the poll next time.
                let prev = cache_lk.replace(s);
                assert!(prev.is_none());
            }
            res
        }
    }
}

/// Create new streams from connection.
impl<B: Buf> OpenStreams<B> for H3Conn {
    type BidiStream = H3Stream;

    type SendStream = H3Stream;

    type OpenError = H3Error;

    fn poll_open_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, Self::OpenError>> {
        #[allow(clippy::let_and_return)]
        let res = Self::poll_open_inner(&mut self.bidi, &self.inner, STREAM_OPEN_FLAG_NONE, cx);
        crate::trace!("msh3 conn poll_open_bidi: {res:?}");
        res
    }

    fn poll_open_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, Self::OpenError>> {
        #[warn(clippy::let_and_return)]
        let res = Self::poll_open_inner(
            &mut self.send,
            &self.inner,
            STREAM_OPEN_FLAG_UNIDIRECTIONAL,
            cx,
        );
        crate::trace!("msh3 conn poll_open_send: {res:?}");
        res
    }

    fn close(&mut self, code: h3::error::Code, _reason: &[u8]) {
        crate::trace!("msh3 conn close");
        let lk = self.inner.as_ref().lock().unwrap();
        lk.shutdown_only(code.value())
    }
}

/// Server accept new streams.
impl<B: Buf> Connection<B> for H3Conn {
    type RecvStream = H3Stream;

    type OpenStreams = H3Conn;

    type AcceptError = H3Error;

    fn poll_accept_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::RecvStream>, Self::AcceptError>> {
        let mut lk = self.inner.as_ref().lock().unwrap();
        // TODO: propogate error.
        #[warn(clippy::let_and_return)]
        let res = lk.poll_accept_uni(cx).map(|x| Ok(x.map(H3Stream::new)));
        crate::trace!("msh3 conn poll_accept_recv: {res:?}");
        res
    }

    fn poll_accept_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::BidiStream>, Self::AcceptError>> {
        let mut lk = self.inner.as_ref().lock().unwrap();
        // TODO: propogate error.
        #[warn(clippy::let_and_return)]
        let res = lk.poll_accept(cx).map(|x| Ok(x.map(H3Stream::new)));
        crate::trace!("msh3 conn poll_accept_bidi {res:?}");
        res
    }

    /// Object to create new streams.
    fn opener(&self) -> Self::OpenStreams {
        crate::trace!("msh3 conn opener");
        self.clone()
    }
}

#[derive(Debug)]
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

    fn get_id(&self) -> h3::quic::StreamId {
        self.inner.get_id().try_into().expect("cannot convert id")
    }

    pub fn get_ref(&self) -> &QStream {
        &self.inner
    }

    #[allow(dead_code)]
    fn get_handle(&self) -> String {
        format!("{:?}", self.get_ref().get_ref().handle)
    }
}

impl<B: Buf> SendStream<B> for H3Stream {
    type Error = H3Error;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        #[warn(clippy::let_and_return)]
        let res = self
            .inner
            .poll_ready_send(cx)
            .map_err(|e| H3Error::new(e, None));
        crate::trace!("msh3 stream [{}] poll_ready {res:?}", self.get_handle());
        res
    }

    fn send_data<T: Into<h3::quic::WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        crate::trace!("msh3 stream [{}] send_data ok", self.get_handle());
        let b: h3::quic::WriteBuf<B> = data.into();
        self.inner.send_only(b, SEND_FLAG_NONE);
        Ok(())
    }

    // send shutdown signal to peer?
    fn poll_finish(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // close the stream
        let do_work = !self.shutdown;
        if do_work {
            self.inner.shutdown_only(STREAM_SHUTDOWN_FLAG_GRACEFUL);
            self.shutdown = true;
        }
        // Does not need to poll? Seems like quinn does not poll this.
        let _res = self
            .inner
            .poll_shutdown(cx)
            .map_err(|e| H3Error::new(e, None));
        crate::trace!(
            "msh3 stream [{}] poll_finish do work ok: {do_work}, {_res:?}",
            self.get_handle()
        );
        Poll::Ready(Ok(()))
    }

    fn reset(&mut self, _reset_code: u64) {
        panic!("reset not supported")
    }

    fn send_id(&self) -> h3::quic::StreamId {
        self.get_id()
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
        #[warn(clippy::let_and_return)]
        let res = match self.inner.poll_receive(cx) {
            std::task::Poll::Ready(br) => Poll::Ready(Ok(br)),
            std::task::Poll::Pending => Poll::Pending,
        };
        crate::trace!(
            "msh3 stream [{}] poll_data pending:{}",
            self.get_handle(),
            res.is_pending()
        );
        res
    }

    fn stop_sending(&mut self, error_code: u64) {
        crate::trace!("msh3 stream [{}] stop_sending ok", self.get_handle());
        self.inner.stop_sending(error_code);
    }

    fn recv_id(&self) -> h3::quic::StreamId {
        self.get_id()
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

#[cfg(test)]
mod tests;
