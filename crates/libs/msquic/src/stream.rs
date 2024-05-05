use std::{
    ffi::c_void,
    io::{Error, ErrorKind},
    slice,
    sync::Mutex,
};

use c2::{
    Handle, SendFlags, Stream, StreamEvent, StreamOpenFlags, StreamStartFlags,
    STREAM_EVENT_PEER_RECEIVE_ABORTED, STREAM_EVENT_PEER_SEND_ABORTED,
    STREAM_EVENT_PEER_SEND_SHUTDOWN, STREAM_EVENT_RECEIVE, STREAM_EVENT_SEND_COMPLETE,
    STREAM_EVENT_SEND_SHUTDOWN_COMPLETE, STREAM_EVENT_SHUTDOWN_COMPLETE,
    STREAM_EVENT_START_COMPLETE, STREAM_SHUTDOWN_FLAG_NONE,
};
use tokio::sync::oneshot;

use crate::{
    buffer::{QBuffRef, QBufferVec, QOwnedBuffer, QVecBuffer},
    conn::QConnection,
    info,
    utils::SBox,
    QApi,
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

#[derive(Debug)]
enum ReceivePayload {
    Success(Vec<QVecBuffer>),
    Stop(ShutdownError),
}

#[derive(Debug, Clone)]
enum SentPayload {
    Success,
    Canceled,
}

#[derive(Debug, Clone)]
enum StartPayload {
    Success,
}

// dummy state lock
enum State {
    Idle,
    Frontend,
    Backend,
}

struct ReceiveState {
    data: Vec<QVecBuffer>, // Buffers received but not yet given to frontend.
    receive_tx: Option<oneshot::Sender<ReceivePayload>>, // frontend pending wait
    is_peer_closed: bool,
}

struct QStreamCtx {
    start_tx: Option<oneshot::Sender<StartPayload>>,
    receive_state: ReceiveState,
    send_tx: Option<oneshot::Sender<SentPayload>>,
    send_shtdwn_tx: Option<oneshot::Sender<()>>,
    drain_tx: Option<oneshot::Sender<()>>,
    is_drained: bool,
    state: Mutex<State>,
}

impl QStreamCtx {
    fn new() -> Self {
        Self {
            //start_rx: Some(start_rx),
            start_tx: None,
            receive_state: ReceiveState {
                data: Vec::new(),
                receive_tx: None,
                is_peer_closed: false,
            },
            send_tx: None,
            send_shtdwn_tx: None,
            drain_tx: None,
            is_drained: false,
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
    let status = 0;
    let mut state = ctx.state.lock().unwrap();
    *state = State::Backend;

    match event.event_type {
        STREAM_EVENT_START_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_START_COMPLETE", stream);
            let tx = ctx.start_tx.take().unwrap();
            tx.send(StartPayload::Success).unwrap();
        }
        STREAM_EVENT_SEND_COMPLETE => {
            let raw = unsafe { event.payload.send_complete };
            info!("[{:?}] STREAM_EVENT_SEND_COMPLETE", stream);
            let tx = ctx.send_tx.take().unwrap();
            let payload = if raw.canceled {
                SentPayload::Canceled
            } else {
                SentPayload::Success
            };
            tx.send(payload).unwrap();
        }
        STREAM_EVENT_RECEIVE => {
            info!("[{:?}] QUIC_STREAM_EVENT_RECEIVE", stream);
            let raw = unsafe { event.payload.receive };
            let count = raw.buffer_count;
            let curr = raw.buffer;
            let buffs = unsafe { slice::from_raw_parts(curr, count.try_into().unwrap()) };
            // send to frontend
            let mut v = buffs
                .iter()
                .map(|b| QVecBuffer::from(&QBuffRef::from(b)))
                .collect::<Vec<_>>();
            if let Some(tx) = ctx.receive_state.receive_tx.take() {
                assert_eq!(ctx.receive_state.data.len(), 0);
                // frontent is waiting
                tx.send(ReceivePayload::Success(v)).unwrap();
                // status = QUIC_STATUS_PENDING;
            } else {
                // queue the buffer to ctx
                ctx.receive_state.data.append(&mut v)
            }
        }
        // peer can shutdown their direction. But we should receive what is pending.
        STREAM_EVENT_PEER_SEND_SHUTDOWN => {
            info!("[{:?}] STREAM_EVENT_PEER_SEND_SHUTDOWN", stream);
            ctx.receive_state.is_peer_closed = true;
            let err = ShutdownError::PeerSendShutdown;
            // Peer will no longer send new stuff, so the receive can be dropped.
            // if frontend is waiting stop it.
            if let Some(tx) = ctx.receive_state.receive_tx.take() {
                tx.send(ReceivePayload::Stop(err)).unwrap();
            }
        }
        STREAM_EVENT_PEER_SEND_ABORTED => {
            info!("[{:?}] STREAM_EVENT_PEER_SEND_ABORTED", stream);
            ctx.receive_state.is_peer_closed = true;
            let err = ShutdownError::PeerSendAbort;
            if let Some(tx) = ctx.receive_state.receive_tx.take() {
                tx.send(ReceivePayload::Stop(err)).unwrap();
            }
        }
        STREAM_EVENT_SEND_SHUTDOWN_COMPLETE => {
            // can ignore for now. This send to peer shutdown?
            info!("[{:?}] STREAM_EVENT_SEND_SHUTDOWN_COMPLETE", stream);
            if let Some(tx) = ctx.send_shtdwn_tx.take() {
                tx.send(()).unwrap();
            }
        }
        STREAM_EVENT_SHUTDOWN_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_SHUTDOWN_COMPLETE", stream);
            // close all channels
            ctx.receive_state.is_peer_closed = true;
            let err = ShutdownError::ShutdownComplete;
            if let Some(tx) = ctx.receive_state.receive_tx.take() {
                tx.send(ReceivePayload::Stop(err)).unwrap();
            }
            // drain signal
            ctx.is_drained = true;
            if let Some(tx) = ctx.drain_tx.take() {
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
        // regardless of start success of fail, there is a QUIC_STREAM_EVENT_START_COMPLETE callback.
        let (start_tx, start_rx) = oneshot::channel();
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;

            // prepare the channel.
            assert!(self.ctx.start_tx.is_none());
            self.ctx.start_tx.replace(start_tx);
            self.inner.inner.start(flags);
        }
        // wait for backend
        match start_rx.await.unwrap() {
            StartPayload::Success => Ok(()),
        }
    }

    // receive into this buff
    // return num of bytes wrote.
    pub async fn receive(&mut self, buff: &mut [u8]) -> Result<u32, Error> {
        let fu;
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            let r_state = &mut self.ctx.receive_state;
            if r_state.data.is_empty() {
                if r_state.is_peer_closed {
                    return Err(Error::from(ErrorKind::BrokenPipe));
                }
                //
                // need to wait for more.
                let (receive_tx, receive_rx) = oneshot::channel();
                assert!(r_state.receive_tx.is_none());
                r_state.receive_tx.replace(receive_tx);
                fu = receive_rx;
            } else {
                let v = &r_state.data;
                let copied = QStream::copy_vec(v, buff);
                assert_ne!(copied, 0);
                return Ok(copied);
            }
        }

        let payload = fu.await.unwrap();
        let v = match payload {
            ReceivePayload::Success(buff) => Ok(buff),
            ReceivePayload::Stop(_) => Err(Error::from(ErrorKind::ConnectionAborted)),
        }?;
        let copied = QStream::copy_vec(&v, buff);
        // resume
        // self.receive_complete(copied as u64);
        Ok(copied)
    }

    fn copy_vec(src: &Vec<QVecBuffer>, buff: &mut [u8]) -> u32 {
        // chain all buff into a single iter
        let mut it = buff.iter_mut();
        let mut copied = 0_u32;
        for b in src {
            let buf_ref = b.as_buff_ref();
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
        copied
    }

    // fn receive_complete(&self, len: u64) {
    //     // TODO: handle error
    //     let _ = self.inner.inner.receive_complete(len);
    // }

    pub async fn send(&mut self, buffers: &[QVecBuffer], flags: SendFlags) -> Result<(), Error> {
        let (send_tx, send_rx) = oneshot::channel();
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;

            assert!(self.ctx.send_tx.is_none());
            self.ctx.send_tx.replace(send_tx);

            let b = QBufferVec::from(buffers);
            let bb = b.as_buffers();
            self.inner
                .inner
                .send(&bb[0], bb.len() as u32, flags, std::ptr::null());
        }

        // wait backend
        send_rx
            .await
            .map_or(Err(Error::from(ErrorKind::BrokenPipe)), Ok)?;
        Ok(())
    }

    // do not call this if already indicated shutdown during send.
    pub async fn shutdown(&mut self) {
        let fu;
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            let (send_shtdwn_tx, send_shtdwn_rx) = oneshot::channel();
            assert!(self.ctx.send_shtdwn_tx.is_none());
            self.ctx.send_shtdwn_tx.replace(send_shtdwn_tx);
            fu = send_shtdwn_rx;
            self.inner.inner.shutdown(STREAM_SHUTDOWN_FLAG_NONE, 0);
        }
        fu.await.unwrap();
    }

    // wait for the complete shutdown event. before close handle.
    pub async fn drain(&mut self) {
        let fu;
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            if self.ctx.is_drained {
                return;
            }
            let (drain_tx, drain_rx) = oneshot::channel();
            assert!(self.ctx.drain_tx.is_none());
            self.ctx.drain_tx.replace(drain_tx);
            fu = drain_rx;
        }
        fu.await.unwrap();
    }
}
