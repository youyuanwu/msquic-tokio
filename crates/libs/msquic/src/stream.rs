use std::{
    ffi::c_void,
    io::{Error, ErrorKind},
    slice,
    sync::Mutex,
};

use crate::{
    buffer::{QBufWrap, QBytesMut},
    conn::QConnection,
    info,
    sync::{QQueue, QResetChannel, QSignal},
    utils::SBox,
    QApi,
};
use bytes::Buf;
use c2::{
    Buffer, Handle, SendFlags, Stream, StreamEvent, StreamOpenFlags, StreamStartFlags,
    STREAM_EVENT_PEER_RECEIVE_ABORTED, STREAM_EVENT_PEER_SEND_ABORTED,
    STREAM_EVENT_PEER_SEND_SHUTDOWN, STREAM_EVENT_RECEIVE, STREAM_EVENT_SEND_COMPLETE,
    STREAM_EVENT_SEND_SHUTDOWN_COMPLETE, STREAM_EVENT_SHUTDOWN_COMPLETE,
    STREAM_EVENT_START_COMPLETE, STREAM_SHUTDOWN_FLAG_NONE,
};

// #[derive(Debug)]
pub struct QStream {
    _api: QApi,
    inner: SBox<Stream>,
    ctx: Box<Mutex<QStreamCtx>>,
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
    start_sig: QResetChannel<StartPayload>,
    receive_ch: QQueue<QBytesMut>,
    send_ch: QResetChannel<SentPayload>,
    send_shtdwn_sig: QSignal,
    drain_sig: QSignal,
    is_drained: bool,
}

impl QStreamCtx {
    fn new() -> Self {
        Self {
            start_sig: QResetChannel::new(),
            receive_ch: QQueue::new(),
            send_ch: QResetChannel::new(),
            send_shtdwn_sig: QSignal::new(),
            drain_sig: QSignal::new(),
            is_drained: false,
        }
    }

    fn on_start_complete(&mut self) {
        if self.start_sig.can_set() {
            self.start_sig.set(StartPayload::Success);
        }
    }
    fn on_send_complete(&mut self, cancelled: bool) {
        let payload = if cancelled {
            SentPayload::Canceled
        } else {
            SentPayload::Success
        };
        if self.send_ch.can_set() {
            self.send_ch.set(payload);
        }
    }
    fn on_receive(&mut self, buffs: &[Buffer]) {
        // send to frontend
        let v = QBytesMut::from_buffs(buffs);
        self.receive_ch.insert(v);
    }
    fn on_peer_send_shutdown(&mut self) {
        // peer can shutdown their direction. But we should receive what is pending.
        // Peer will no longer send new stuff, so the receive can be dropped.
        // if frontend is waiting stop it.
        self.receive_ch.close(0);
    }
    fn on_peer_send_abort(&mut self, _ec: u64) {
        self.receive_ch.close(0);
    }
    fn on_send_shutdown_complete(&mut self) {
        if self.send_shtdwn_sig.can_set() {
            self.send_shtdwn_sig.set(());
        }
    }
    fn on_shutdown_complete(&mut self) {
        // close all channels
        self.receive_ch.close(0);
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
    let mut ctx = ctx.lock().unwrap();
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
            info!("[{:?}] QUIC_STREAM_EVENT_RECEIVE", stream);
            let raw = unsafe { event.payload.receive };
            let count = raw.buffer_count;
            let curr = raw.buffer;
            let buffs = unsafe { slice::from_raw_parts(curr, count.try_into().unwrap()) };
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
    pub fn attach(api: QApi, h: Handle) -> Self {
        let s = Stream::from_parts(h, &api.inner.inner);
        let ctx = Box::new(Mutex::new(QStreamCtx::new()));
        s.set_callback_handler(
            qstream_handler_callback,
            &*ctx as *const Mutex<QStreamCtx> as *const c_void,
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
        let ctx = Box::new(Mutex::new(QStreamCtx::new()));
        s.open(
            &connection.inner.inner,
            flags,
            qstream_handler_callback,
            &*ctx as *const Mutex<QStreamCtx> as *const c_void,
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
        let rx;
        {
            // prepare the channel.
            rx = self.ctx.lock().unwrap().start_sig.reset();
            self.inner.inner.start(flags);
        }
        // wait for backend
        match rx.await {
            StartPayload::Success => Ok(()),
        }
    }

    // receive into this buff
    // return num of bytes wrote.
    pub async fn receive(&mut self) -> Result<impl Buf, Error> {
        let rx;
        {
            rx = self.ctx.lock().unwrap().receive_ch.pop();
        }

        let v = rx
            .await
            .map_err(|e: u32| Error::from_raw_os_error(e.try_into().unwrap()))?;
        Ok(v.0)
    }

    // fn receive_complete(&self, len: u64) {
    //     // TODO: handle error
    //     let _ = self.inner.inner.receive_complete(len);
    // }

    pub async fn send(&mut self, buffers: impl Buf, flags: SendFlags) -> Result<(), Error> {
        let b = QBufWrap::new(buffers);
        let rx;
        {
            let bb = b.as_buffs();
            rx = self.ctx.lock().unwrap().send_ch.reset();
            self.inner
                .inner
                .send(&bb[0], bb.len() as u32, flags, std::ptr::null());
        }

        // wait backend
        let res = rx.await;
        match res {
            SentPayload::Success => Ok(()),
            SentPayload::Canceled => Err(Error::from(ErrorKind::ConnectionAborted)),
        }
    }

    // send shutdown signal to peer.
    // do not call this if already indicated shutdown during send.
    pub async fn shutdown(&mut self) {
        let rx;
        {
            rx = self.ctx.lock().unwrap().send_shtdwn_sig.reset();
            self.inner.inner.shutdown(STREAM_SHUTDOWN_FLAG_NONE, 0);
        }
        rx.await;
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
}
