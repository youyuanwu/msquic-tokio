use std::{
    ffi::c_void,
    io::{Error, ErrorKind},
    slice,
};

use c2::{
    Buffer, Handle, SendFlags, Stream, StreamEvent, StreamOpenFlags, StreamStartFlags,
    STREAM_EVENT_PEER_RECEIVE_ABORTED, STREAM_EVENT_PEER_SEND_ABORTED,
    STREAM_EVENT_PEER_SEND_SHUTDOWN, STREAM_EVENT_RECEIVE, STREAM_EVENT_SEND_COMPLETE,
    STREAM_EVENT_SEND_SHUTDOWN_COMPLETE, STREAM_EVENT_SHUTDOWN_COMPLETE,
    STREAM_EVENT_START_COMPLETE,
};
use tokio::sync::{mpsc, oneshot};

use crate::{
    buffer::{QBuffRef, QBufferVec, QVecBuffer, SBuffer},
    conn::QConnection,
    info,
    utils::SBox,
    QApi, QUIC_STATUS_PENDING,
};

pub struct QStream {
    _api: QApi,
    inner: SBox<Stream>,
    ctx: Box<QStreamCtx>,
}

struct QStreamCtx {
    start_tx: Option<oneshot::Sender<()>>,
    start_rx: Option<oneshot::Receiver<()>>,
    receive_tx: mpsc::Sender<Vec<SBuffer>>,
    receive_rx: mpsc::Receiver<Vec<SBuffer>>,
    send_tx: mpsc::Sender<()>,
    send_rx: mpsc::Receiver<()>,
}

impl QStreamCtx {
    fn new() -> Self {
        let (start_tx, start_rx) = oneshot::channel();
        let (receive_tx, receive_rx) = mpsc::channel(1);
        let (send_tx, send_rx) = mpsc::channel(1);
        Self {
            start_rx: Some(start_rx),
            start_tx: Some(start_tx),
            receive_rx,
            receive_tx,
            send_rx,
            send_tx,
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

    match event.event_type {
        STREAM_EVENT_START_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_START_COMPLETE", stream);
            ctx.start_tx.take().unwrap().send(()).unwrap();
        }
        STREAM_EVENT_SEND_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_START_COMPLETE", stream);
            ctx.send_tx.blocking_send(()).unwrap();
        }
        STREAM_EVENT_RECEIVE => {
            info!("[{:?}] QUIC_STREAM_EVENT_RECEIVE", stream);
            let raw = unsafe { event.payload.receive };
            let count = raw.buffer_count;
            let curr = raw.buffer;
            let buffs = unsafe { slice::from_raw_parts(curr, count.try_into().unwrap()) };
            // send to frontend
            let v = buffs.iter().map(SBuffer::from).collect();
            ctx.receive_tx.blocking_send(v).unwrap();
            status = QUIC_STATUS_PENDING;
        }
        STREAM_EVENT_PEER_SEND_SHUTDOWN => {
            info!("[{:?}] STREAM_EVENT_PEER_SEND_SHUTDOWN", stream);
        }
        STREAM_EVENT_PEER_SEND_ABORTED => {
            info!("[{:?}] STREAM_EVENT_PEER_SEND_ABORTED", stream);
        }
        STREAM_EVENT_SEND_SHUTDOWN_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_SEND_SHUTDOWN_COMPLETE", stream);
        }
        STREAM_EVENT_SHUTDOWN_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_SHUTDOWN_COMPLETE", stream);
        }
        STREAM_EVENT_PEER_RECEIVE_ABORTED => {
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
    pub async fn start(&mut self, flags: StreamStartFlags) {
        self.inner.inner.start(flags);
        // wait for backend
        self.ctx.start_rx.take().unwrap().await.unwrap()
    }

    // receive into this buff
    // return num of bytes wrote.
    pub async fn receive(&mut self, buff: &mut [u8]) -> Result<u32, Error> {
        let v = self
            .ctx
            .receive_rx
            .recv()
            .await
            .map_or(Err(Error::from(ErrorKind::UnexpectedEof)), Ok)?;
        // chain all buff into a single iter
        let mut it = buff.iter_mut();
        let copied = 0_u32;
        for b in &v {
            let buf_ref = QBuffRef::from(&Buffer::from(b));
            buf_ref.data.iter().enumerate().for_each(|(_, val)| loop {
                let op = it.next();
                match op {
                    Some(x) => {
                        *x = *val;
                    }
                    None => {
                        break;
                    }
                }
            });
        }

        // resume
        self.receive_complete(copied as u64);
        Ok(copied)
    }

    fn receive_complete(&self, len: u64) {
        self.inner.inner.receive_complete(len)
    }

    pub async fn send(&mut self, buffers: &[QVecBuffer], flags: SendFlags) -> Result<(), Error> {
        {
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

    // pub async fn shutdown(&self) -> Result<(), Error> {
    //     self.inner.inner.
    // }
}
