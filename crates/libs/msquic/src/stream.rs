use std::{
    ffi::c_void,
    io::{Error, ErrorKind},
    slice,
    sync::Mutex,
};

use c2::{
    Buffer, Handle, SendFlags, Stream, StreamEvent, StreamOpenFlags, StreamStartFlags,
    STREAM_EVENT_PEER_RECEIVE_ABORTED, STREAM_EVENT_PEER_SEND_ABORTED,
    STREAM_EVENT_PEER_SEND_SHUTDOWN, STREAM_EVENT_RECEIVE, STREAM_EVENT_SEND_COMPLETE,
    STREAM_EVENT_SEND_SHUTDOWN_COMPLETE, STREAM_EVENT_SHUTDOWN_COMPLETE,
    STREAM_EVENT_START_COMPLETE, STREAM_SHUTDOWN_FLAG_NONE,
};
use tokio::sync::{mpsc, oneshot};

use crate::{
    buffer::{QBuffRef, QBufferVec, QVecBuffer, SBuffer},
    conn::QConnection,
    info,
    utils::SBox,
    QApi, QUIC_STATUS_PENDING,
};

// #[derive(Debug)]
pub struct QStream {
    _api: QApi,
    inner: SBox<Stream>,
    ctx: Box<QStreamCtx>,
}

#[derive(Debug, Clone)]
enum ShutdownError {
    PeerSendShutdown,
    PeerSendAbort,
    ShutdownComplete,
}

enum ReceivePayload {
    Success(Vec<SBuffer>),
    Stop(ShutdownError),
}

#[derive(Debug, Clone)]
enum SentPayload {
    Success,
    Stop(ShutdownError),
}

#[derive(Debug, Clone)]
enum StartPayload {
    Success,
    Stop(ShutdownError),
}

// dummy state lock
enum State {
    Idle,
    Frontend,
    Backend,
}

struct QStreamCtx {
    start_tx: Option<oneshot::Sender<StartPayload>>,
    start_rx: Option<oneshot::Receiver<StartPayload>>,
    receive_tx: Option<mpsc::Sender<ReceivePayload>>,
    receive_rx: mpsc::Receiver<ReceivePayload>,
    send_tx: Option<mpsc::Sender<SentPayload>>,
    send_rx: mpsc::Receiver<SentPayload>,
    shtdwn_tx: Option<oneshot::Sender<()>>,
    shtdwn_rx: Option<oneshot::Receiver<()>>,
    state: Mutex<State>,
}

impl QStreamCtx {
    fn new() -> Self {
        let (start_tx, start_rx) = oneshot::channel();
        let (shtdwn_tx, shtdwn_rx) = oneshot::channel();
        let (receive_tx, receive_rx) = mpsc::channel(1);
        let (send_tx, send_rx) = mpsc::channel(1);
        Self {
            start_rx: Some(start_rx),
            start_tx: Some(start_tx),
            receive_rx,
            receive_tx: Some(receive_tx),
            send_rx,
            send_tx: Some(send_tx),
            shtdwn_tx: Some(shtdwn_tx),
            shtdwn_rx: Some(shtdwn_rx),
            state: Mutex::new(State::Idle),
        }
    }
}

extern "C" fn qstream_handler_callback(
    stream: Handle,
    context: *mut c_void,
    event: &StreamEvent,
) -> u32 {
    assert!(!context.is_null());
    let ctx = unsafe { (context as *mut QStreamCtx).as_mut().unwrap() };
    let mut status = 0;
    let mut state = ctx.state.lock().unwrap();
    *state = State::Backend;

    match event.event_type {
        STREAM_EVENT_START_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_START_COMPLETE", stream);
            if let Some(tx) = ctx.start_tx.take() {
                tx.send(StartPayload::Success).unwrap();
            }
        }
        STREAM_EVENT_SEND_COMPLETE => {
            //let raw = event.payload.send_complete
            info!("[{:?}] STREAM_EVENT_START_COMPLETE", stream);
            if let Some(tx) = ctx.send_tx.as_mut() {
                tx.blocking_send(SentPayload::Success).unwrap();
            }
        }
        STREAM_EVENT_RECEIVE => {
            info!("[{:?}] QUIC_STREAM_EVENT_RECEIVE", stream);
            let raw = unsafe { event.payload.receive };
            let count = raw.buffer_count;
            let curr = raw.buffer;
            let buffs = unsafe { slice::from_raw_parts(curr, count.try_into().unwrap()) };
            // send to frontend
            let v = buffs.iter().map(SBuffer::from).collect::<Vec<_>>();
            if let Some(tx) = ctx.receive_tx.as_mut() {
                tx.blocking_send(ReceivePayload::Success(v)).unwrap();
                status = QUIC_STATUS_PENDING;
            }
            // else ignore
        }
        // peer can shutdown their direction. But we should receive what is pending.
        STREAM_EVENT_PEER_SEND_SHUTDOWN => {
            info!("[{:?}] STREAM_EVENT_PEER_SEND_SHUTDOWN", stream);
            let err = ShutdownError::PeerSendShutdown;
            // Peer will no longer send new stuff, so the receive can be dropped
            if let Some(tx) = ctx.receive_tx.take() {
                tx.blocking_send(ReceivePayload::Stop(err)).unwrap();
            }
        }
        STREAM_EVENT_PEER_SEND_ABORTED => {
            info!("[{:?}] STREAM_EVENT_PEER_SEND_ABORTED", stream);
            let err = ShutdownError::PeerSendAbort;
            if let Some(tx) = ctx.receive_tx.take() {
                tx.blocking_send(ReceivePayload::Stop(err)).unwrap();
            }
        }
        STREAM_EVENT_SEND_SHUTDOWN_COMPLETE => {
            // can ignore for now. This send to peer shutdown?
            info!("[{:?}] STREAM_EVENT_SEND_SHUTDOWN_COMPLETE", stream);
        }
        STREAM_EVENT_SHUTDOWN_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_SHUTDOWN_COMPLETE", stream);
            // close all channels
            let err = ShutdownError::ShutdownComplete;
            if let Some(tx) = ctx.start_tx.take() {
                tx.send(StartPayload::Stop(err.clone())).unwrap();
            }
            if let Some(tx) = ctx.receive_tx.take() {
                tx.blocking_send(ReceivePayload::Stop(err.clone())).unwrap();
            }
            if let Some(tx) = ctx.send_tx.take() {
                tx.blocking_send(SentPayload::Stop(err)).unwrap();
            }
            if let Some(tx) = ctx.shtdwn_tx.take() {
                tx.send(()).unwrap();
            }
        }
        STREAM_EVENT_PEER_RECEIVE_ABORTED => {
            // can ignore for now
            info!("[{:?}] STREAM_EVENT_PEER_RECEIVE_ABORTED", stream);
        }
        _ => {
            info!("[{:?}] STREAM_EVENT Unknown {}", stream, event.event_type);
        }
    }
    status
}

impl QStream {
    pub fn attach(api: QApi, h: Handle) -> Self {
        let s = Stream::from_parts(h, &api.inner.inner);
        let ctx = Box::new(QStreamCtx::new());
        s.set_callback_handler(
            qstream_handler_callback,
            &*ctx as *const QStreamCtx as *const c_void,
        );

        Self {
            _api: api,
            inner: SBox::new(s),
            ctx,
        }
    }

    // open client stream
    pub fn open(connection: &QConnection, flags: StreamOpenFlags) -> Self {
        let s = Stream::new(&connection._api.inner.inner);
        let ctx = Box::new(QStreamCtx::new());
        s.open(
            &connection.inner.inner,
            flags,
            qstream_handler_callback,
            &*ctx as *const QStreamCtx as *const c_void,
        );
        Self {
            _api: connection._api.clone(),
            ctx,
            inner: SBox::new(s),
        }
    }

    // start stream for client
    pub async fn start(&mut self, flags: StreamStartFlags) -> Result<(), Error> {
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            self.inner.inner.start(flags);
        }
        // wait for backend
        match self.ctx.start_rx.take().unwrap().await.unwrap() {
            StartPayload::Success => Ok(()),
            StartPayload::Stop(_) => Err(Error::from(ErrorKind::ConnectionAborted)),
        }
    }

    // receive into this buff
    // return num of bytes wrote.
    pub async fn receive(&mut self, buff: &mut [u8]) -> Result<u32, Error> {
        let fu;
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            fu = self.ctx.receive_rx.recv();
        }

        let payload = fu.await;
        let v = match payload {
            Some(p) => match p {
                ReceivePayload::Success(buff) => Ok(buff),
                ReceivePayload::Stop(_) => Err(Error::from(ErrorKind::ConnectionAborted)),
            },
            None => Err(Error::from(ErrorKind::ConnectionAborted)),
        }?;
        // chain all buff into a single iter
        let mut it = buff.iter_mut();
        let mut copied = 0_u32;
        for b in &v {
            let buf_ref = QBuffRef::from(&Buffer::from(b));
            let mut src_iter = buf_ref.data.iter();
            loop {
                let dst = it.next();
                let src = src_iter.next();
                if dst.is_none() || src.is_none() {
                    break;
                }
                *dst.unwrap() = *src.unwrap();
                copied += 1;
            }
        }
        // resume
        self.receive_complete(copied as u64);
        Ok(copied)
    }

    fn receive_complete(&self, len: u64) {
        // TODO: handle error
        let _ = self.inner.inner.receive_complete(len);
    }

    pub async fn send(&mut self, buffers: &[QVecBuffer], flags: SendFlags) -> Result<(), Error> {
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            let b = QBufferVec::from(buffers);
            let bb = b.as_buffers();
            self.inner
                .inner
                .send(&bb[0], bb.len() as u32, flags, std::ptr::null());
        }

        // wait backend
        self.ctx
            .send_rx
            .recv()
            .await
            .map_or(Err(Error::from(ErrorKind::UnexpectedEof)), Ok)?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            if self.ctx.shtdwn_tx.is_none() {
                // already shtdwn. no need to trigger.
            } else {
                self.inner.inner.shutdown(STREAM_SHUTDOWN_FLAG_NONE, 0);
            }
        }
        self.ctx.shtdwn_rx.take().unwrap().await.unwrap();
    }
}
