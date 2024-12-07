use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

use crate::conn::QConnection;
use msquic_sys2::{
    Addr, Buffer, Configuration, Handle, Listener, ListenerEvent, LISTENER_EVENT_NEW_CONNECTION,
};

use crate::{config::QConfiguration, reg::QRegistration, utils::SBox, QApi};

pub struct QListener {
    _api: QApi,
    inner: SBox<Listener>,
    ctx: Box<Mutex<QListenerCtx>>, // TODO: mutex may be removed.
    ch_rx: tokio::sync::mpsc::UnboundedReceiver<QConnection>,
}

struct QListenerCtx {
    _api: QApi,
    ch_tx: Option<tokio::sync::mpsc::UnboundedSender<QConnection>>,
    config: Arc<SBox<Configuration>>,
    state: State, // for validation only
}

#[derive(PartialEq, Debug)]
enum State {
    Idle,
    Started,
    StopRequested,
    Stopped,
}

const QUIC_LISTENER_EVENT_STOP_COMPLETE: u32 = 1;

extern "C" fn listener_handler(
    _listener: Handle,
    context: *mut c_void,
    event: &ListenerEvent,
) -> u32 {
    assert!(!context.is_null());
    let ctx = unsafe { (context as *mut Mutex<QListenerCtx>).as_mut().unwrap() };
    #[allow(clippy::mut_mutex_lock)]
    // clippy asks us to borrow because it does not know we are using unsafe
    let mut ctx = ctx.lock().unwrap();
    let status = 0;
    match event.event_type {
        LISTENER_EVENT_NEW_CONNECTION => {
            let raw = unsafe { event.payload.new_connection };
            let h = raw.connection as Handle;
            let _info = unsafe { raw.info.as_ref().unwrap() };
            crate::trace!(
                "[{:?}] LISTENER_EVENT_NEW_CONNECTION conn=[{:?}] info={:?}",
                _listener,
                h,
                _info
            );
            assert_eq!(ctx.state, State::Started);
            ctx.on_new_connection(h);
        }
        QUIC_LISTENER_EVENT_STOP_COMPLETE => {
            crate::trace!("[{:?}] QUIC_LISTENER_EVENT_STOP_COMPLETE", _listener);
            assert_eq!(ctx.state, State::StopRequested);
            ctx.state = State::Stopped;
            // stop the acceptor
            ctx.on_stop_complete();
        }
        _ => {
            unreachable!()
        }
    };

    status
}

impl QListener {
    // server open listener
    pub fn open(registration: &QRegistration, configuration: &QConfiguration) -> Self {
        let (ch_tx, ch_rx) = tokio::sync::mpsc::unbounded_channel();
        let context = Box::new(Mutex::new(QListenerCtx {
            _api: registration.api.clone(),
            config: configuration.inner.clone(),
            state: State::Idle,
            ch_tx: Some(ch_tx),
        }));
        let l = Listener::new(
            &registration.inner.inner,
            listener_handler,
            (&*context) as *const Mutex<QListenerCtx> as *const c_void,
        );
        Self {
            _api: registration.api.clone(),
            inner: SBox { inner: l },
            ctx: context,
            ch_rx,
        }
    }

    pub fn start(&self, alpn: &[Buffer], local_address: &Addr) {
        {
            let mut lk = self.ctx.lock().unwrap();
            assert_eq!(lk.state, State::Idle);
            lk.state = State::Started;
        }
        self.inner.inner.start(alpn, local_address)
    }

    pub async fn accept(&mut self) -> Option<QConnection> {
        self.ch_rx.recv().await
    }

    pub async fn stop(&mut self) {
        {
            let mut lk = self.ctx.lock().unwrap();
            assert_eq!(lk.state, State::Started);
            lk.state = State::StopRequested;
        }
        // callback may be invoked in the same thread.
        crate::trace!("listner stop requested.");
        self.inner.inner.stop();
        crate::trace!("wait for mpsc sender to drop");
        while !self.ch_rx.is_closed() {
            tokio::task::yield_now().await;
        }
        crate::trace!("wait for mpsc sender to drop ok.");
    }
}

impl QListenerCtx {
    fn on_new_connection(&mut self, conn: Handle) {
        let c = QConnection::attach(self._api.clone(), conn, &self.config.inner);
        self.ch_tx
            .as_ref()
            .expect("sender already closed")
            .send(c)
            .expect("receiver should not be closed");
    }

    fn on_stop_complete(&mut self) {
        self.ch_tx = None; // this drops the sender.
    }
}
