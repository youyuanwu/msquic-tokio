use std::{
    ffi::c_void,
    fmt::Debug,
    future::poll_fn,
    io::{Error, ErrorKind},
    slice,
    sync::{Arc, Mutex, MutexGuard},
    task::Poll,
};

use crate::{
    buffer::{QBufWrap, QBytesMut},
    conn::QConnection,
    info,
    sync::{QSignal, QWakableQueue, QWakableSig},
    utils::SBox,
    QApi, QUIC_STATUS_SUCCESS,
};
use bytes::{Buf, BytesMut};
use c2::{
    Buffer, Handle, SendFlags, Stream, StreamEvent, StreamOpenFlags, StreamShutdownFlags,
    StreamStartFlags, STREAM_EVENT_PEER_RECEIVE_ABORTED, STREAM_EVENT_PEER_SEND_ABORTED,
    STREAM_EVENT_PEER_SEND_SHUTDOWN, STREAM_EVENT_RECEIVE, STREAM_EVENT_SEND_COMPLETE,
    STREAM_EVENT_SEND_SHUTDOWN_COMPLETE, STREAM_EVENT_SHUTDOWN_COMPLETE,
    STREAM_EVENT_START_COMPLETE, STREAM_SHUTDOWN_FLAG_GRACEFUL, STREAM_SHUTDOWN_FLAG_NONE,
};

#[derive(Clone)]
pub struct QStream {
    _api: QApi,
    inner: Arc<SBox<Stream>>, // arc needed for copy
    ctx: Arc<Mutex<QStreamCtx>>,
}

impl Debug for QStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QStream")
            .field("_api", &"skip")
            .field("inner", &self.inner)
            .field("ctx", &"skip")
            .finish()
    }
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

struct QStreamCtx {
    start_sig: QWakableQueue<StartPayload>,
    //receive_ch: QWakableQueue<QBytesMut>, // TODO: change to mpsc
    receive_tx: Option<tokio::sync::mpsc::UnboundedSender<QBytesMut>>,
    receive_rx: tokio::sync::mpsc::UnboundedReceiver<QBytesMut>,
    send_sig: QWakableSig<SentPayload>,
    send_shtdwn_sig: QWakableSig<()>,
    drain_sig: QSignal,
    is_drained: bool,
    pending_buf: Option<QBufWrap>, // because msquic copies buffers in background we need to hold the buffer temporarily
}

impl QStreamCtx {
    fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            start_sig: QWakableQueue::default(),
            // receive_ch: QWakableQueue::default(),
            send_sig: QWakableSig::default(),
            send_shtdwn_sig: QWakableSig::default(),
            drain_sig: QSignal::new(),
            is_drained: false,
            pending_buf: None,
            receive_tx: Some(tx),
            receive_rx: rx,
        }
    }

    fn on_start_complete(&mut self) {
        self.start_sig.insert(StartPayload::Success);
    }
    fn on_send_complete(&mut self, cancelled: bool) {
        let payload = if cancelled {
            SentPayload::Canceled
        } else {
            SentPayload::Success
        };
        let prev = self.pending_buf.take(); // release buffer
        assert!(prev.is_some());
        self.send_sig.set(payload);
    }
    fn on_receive(&mut self, buffs: &[Buffer]) {
        if buffs.is_empty() {
            // sometimes msquic calls with 0 buffer.
            // we ignore it because h3 does not handle it.
            return;
        }
        // send to frontend
        let v = QBytesMut::from_buffs(buffs);
        // let s = debug_buf_to_string(v.0.clone());
        // let original = debug_raw_buf_to_string(buffs[0]);
        // info!(
        //     "debug: receive bytes: {} len:{}, original {}, len: {}",
        //     s,
        //     s.len(),
        //     original,
        //     original.len()
        // );
        self.receive_tx
            .as_ref()
            .expect("tx none")
            .send(v)
            .expect("fail send");
    }
    fn on_peer_send_shutdown(&mut self) {
        // peer can shutdown their direction. But we should receive what is pending.
        // Peer will no longer send new stuff, so the receive can be dropped.
        // if frontend is waiting stop it.
        self.receive_tx = None;
    }
    fn on_peer_send_abort(&mut self, _ec: u64) {
        self.receive_tx = None;
    }
    fn on_send_shutdown_complete(&mut self) {
        self.send_shtdwn_sig.set(());
    }
    fn on_shutdown_complete(&mut self) {
        // close all channels
        self.receive_tx = None;
        self.send_shtdwn_sig.close();
        // drain signal
        self.is_drained = true;
        if self.drain_sig.can_set() {
            self.drain_sig.set(())
        }
    }
}

extern "C" fn qstream_handler_callback(
    stream: Handle,
    context: *mut c_void,
    event: &StreamEvent,
) -> u32 {
    assert!(!context.is_null());
    let ctx = unsafe { (context as *mut Mutex<QStreamCtx>).as_mut().unwrap() };
    #[allow(clippy::mut_mutex_lock)]
    let mut ctx = match ctx.lock() {
        Ok(c) => c,
        Err(_) => {
            // use after free.
            tracing::error!("[{:?}] event shoud panic: {:?}", stream, event.event_type);
            return QUIC_STATUS_SUCCESS;
        }
    };
    let status = 0;

    match event.event_type {
        STREAM_EVENT_START_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_START_COMPLETE", stream);
            ctx.on_start_complete();
        }
        STREAM_EVENT_SEND_COMPLETE => {
            let raw = unsafe { event.payload.send_complete };
            info!(
                "[{:?}] STREAM_EVENT_SEND_COMPLETE cancel {}",
                stream, raw.canceled
            );
            ctx.on_send_complete(raw.canceled);
        }
        STREAM_EVENT_RECEIVE => {
            let raw = unsafe { event.payload.receive };
            let count = raw.buffer_count;
            let curr = raw.buffer;
            let buffs = unsafe { slice::from_raw_parts(curr, count.try_into().unwrap()) };
            info!(
                "[{:?}] QUIC_STREAM_EVENT_RECEIVE: buffer count {}",
                stream,
                buffs.len(),
            );
            ctx.on_receive(buffs);
        }
        STREAM_EVENT_PEER_SEND_SHUTDOWN => {
            info!("[{:?}] STREAM_EVENT_PEER_SEND_SHUTDOWN", stream);
            ctx.on_peer_send_shutdown();
        }
        STREAM_EVENT_PEER_SEND_ABORTED => {
            let raw = unsafe { event.payload.peer_send_aborted };
            info!(
                "[{:?}] STREAM_EVENT_PEER_SEND_ABORTED: ec {}",
                stream, raw.error_code
            );
            ctx.on_peer_send_abort(raw.error_code);
        }
        STREAM_EVENT_SEND_SHUTDOWN_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_SEND_SHUTDOWN_COMPLETE", stream);
            ctx.on_send_shutdown_complete();
        }
        STREAM_EVENT_SHUTDOWN_COMPLETE => {
            info!("[{:?}] STREAM_EVENT_SHUTDOWN_COMPLETE", stream);
            ctx.on_shutdown_complete();
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
    pub fn get_ref(&self) -> &Stream {
        self.inner.as_ref().get_ref()
    }

    pub fn attach(api: QApi, h: Handle) -> Self {
        let s = Stream::from_parts(h, &api.inner.inner);
        let ctx = Arc::new(Mutex::new(QStreamCtx::new()));
        s.set_callback_handler(
            qstream_handler_callback,
            &*ctx as *const Mutex<QStreamCtx> as *const c_void,
        );

        Self {
            _api: api,
            inner: Arc::new(SBox::new(s)),
            ctx,
        }
    }

    // open client stream
    pub fn open(connection: &QConnection, flags: StreamOpenFlags) -> Self {
        let s = Stream::new(&connection._api.inner.inner);
        let ctx = Arc::new(Mutex::new(QStreamCtx::new()));
        s.open(
            &connection.inner.inner,
            flags,
            qstream_handler_callback,
            &*ctx as *const Mutex<QStreamCtx> as *const c_void,
        );
        Self {
            _api: connection._api.clone(),
            ctx,
            inner: Arc::new(SBox::new(s)),
        }
    }

    pub fn start_only(&self, flags: StreamStartFlags) {
        self.inner.inner.start(flags);
    }

    pub fn poll_start(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Error>> {
        let p = self.ctx.lock().unwrap().start_sig.poll(cx);
        match p {
            std::task::Poll::Ready(op) => match op {
                Some(_) => Poll::Ready(Ok(())),
                None => Poll::Ready(Err(Error::from(ErrorKind::BrokenPipe))),
            },
            std::task::Poll::Pending => Poll::Pending,
        }
    }

    // start stream for client
    pub async fn start(&mut self, flags: StreamStartFlags) -> Result<(), Error> {
        // regardless of start success of fail, there is a QUIC_STREAM_EVENT_START_COMPLETE callback.
        self.start_only(flags);
        let fu = poll_fn(|cx| self.poll_start(cx));
        fu.await
    }

    // todo: propagate error
    pub fn poll_receive(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<BytesMut>> {
        let p = self.ctx.lock().unwrap().receive_rx.poll_recv(cx);
        match p {
            Poll::Ready(op) => match op {
                Some(b) => Poll::Ready(Some(b.0)),
                None => Poll::Ready(None),
            },
            Poll::Pending => Poll::Pending,
        }
    }

    // receive into this buff
    // return num of bytes wrote.
    pub async fn receive(&mut self) -> Option<BytesMut> {
        let fu = poll_fn(|cx| self.poll_receive(cx));
        fu.await
    }

    // fn receive_complete(&self, len: u64) {
    //     // TODO: handle error
    //     let _ = self.inner.inner.receive_complete(len);
    // }
    pub fn send_only(&mut self, buffers: impl Buf, flags: SendFlags) {
        let mut lk = self.ctx.lock().unwrap();
        lk.send_sig.set_frontend_pending();
        let b = QBufWrap::new(Box::new(buffers));
        // hold on the buffer until callback.
        let prev = lk.pending_buf.replace(b);
        assert!(prev.is_none());
        let bb = lk.pending_buf.as_ref().unwrap().as_buffs();
        self.inner
            .inner
            .send(&bb[0], bb.len() as u32, flags, std::ptr::null());
    }

    pub fn poll_send(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Error>> {
        let mut p = self.ctx.lock().unwrap();
        Self::poll_send_inner(&mut p, cx)
    }

    fn poll_send_inner(
        lk: &mut MutexGuard<QStreamCtx>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Error>> {
        let p = lk.send_sig.poll(cx);
        match p {
            std::task::Poll::Ready(op) => match op {
                Some(e) => match e {
                    SentPayload::Success => Poll::Ready(Ok(())),
                    SentPayload::Canceled => {
                        Poll::Ready(Err(Error::from(ErrorKind::ConnectionAborted)))
                    }
                },
                None => Poll::Ready(Err(Error::from(ErrorKind::BrokenPipe))),
            },
            std::task::Poll::Pending => Poll::Pending,
        }
    }

    pub async fn send(&mut self, buffers: impl Buf, flags: SendFlags) -> Result<(), Error> {
        self.send_only(buffers, flags);
        let fu = poll_fn(|cx| self.poll_send(cx));
        fu.await
    }

    // poll if send is ready for more data
    pub fn poll_ready_send(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Error>> {
        let mut lk = self.ctx.lock().unwrap();
        // If frontend pending is not yet cleared from backend, we set waker in backend.
        if !lk.send_sig.is_frontend_pending() {
            return Poll::Ready(Ok(()));
        }
        Self::poll_send_inner(&mut lk, cx)
    }

    pub fn shutdown_only(&mut self, flags: StreamShutdownFlags) {
        let mut lk = self.ctx.lock().unwrap();
        lk.send_shtdwn_sig.reset();
        self.inner.inner.shutdown(flags, 0);
    }

    pub fn poll_shutdown(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), Error>> {
        let mut lk = self.ctx.lock().unwrap();
        let p = lk.send_shtdwn_sig.poll(cx);
        match p {
            Poll::Ready(_) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }

    // send shutdown signal to peer.
    // do not call this if already indicated shutdown during send.
    pub async fn shutdown(&mut self) -> Result<(), Error> {
        self.shutdown_only(STREAM_SHUTDOWN_FLAG_NONE);
        let fu = poll_fn(|cx| self.poll_shutdown(cx));
        fu.await
    }

    // this is for h3 where the interface does not wait
    // We will ignore callback.
    pub fn stop_sending(&self, error_code: u64) {
        self.inner
            .inner
            .shutdown(STREAM_SHUTDOWN_FLAG_GRACEFUL, error_code)
    }

    // wait for the complete shutdown event. before close handle.
    pub async fn drain(&mut self) {
        let rx;
        {
            let mut lk = self.ctx.lock().unwrap();
            if lk.is_drained {
                return;
            }
            rx = lk.drain_sig.reset();
        }
        rx.await;
    }

    // get stream id
    pub fn get_id(&self) -> u64 {
        self.inner.inner.get_id()
    }
}

impl Drop for QStream {
    // force to wait stream shutdown. TODO:
    fn drop(&mut self) {
        let h = self.inner.inner.handle;
        tracing::info!("Stream drop [{:?}]", h);
        // let lk = self.ctx.lock().unwrap();
        // if !lk.is_drained{
        //     panic!("drop happens before shutdown callback");
        // }
        // loop {
        //     let lk = self.ctx.lock().unwrap();
        //     match lk.is_drained {
        //         true => break,
        //         false => std::thread::sleep(std::time::Duration::from_nanos(10)),
        //     }
        // }
    }
}
