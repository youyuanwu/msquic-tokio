use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

use crate::{
    conn::QConnection,
    info,
    sync::{QQueue, QResetChannel, QSignal},
};
use c2::{
    Addr, Buffer, Configuration, Handle, Listener, ListenerEvent, LISTENER_EVENT_NEW_CONNECTION,
};

use crate::{config::QConfiguration, reg::QRegistration, utils::SBox, QApi};

pub struct QListener {
    _api: QApi,
    inner: SBox<Listener>,
    ctx: Box<Mutex<QListenerCtx>>,
}

struct QListenerCtx {
    _api: QApi,
    ch: QQueue<QConnection>,
    config: Arc<SBox<Configuration>>,
    state: State, // for validation only
    sig_stop: QSignal,
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
    listener: Handle,
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
            let info = unsafe { raw.info.as_ref().unwrap() };
            info!(
                "[{:?}] LISTENER_EVENT_NEW_CONNECTION conn=[{:?}] info={:?}",
                listener, h, info
            );
            assert_eq!(ctx.state, State::Started);
            ctx.on_new_connection(h);
        }
        QUIC_LISTENER_EVENT_STOP_COMPLETE => {
            info!("[{:?}] QUIC_LISTENER_EVENT_STOP_COMPLETE", listener);
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
        let context = Box::new(Mutex::new(QListenerCtx {
            _api: registration.api.clone(),
            ch: QQueue::new(),
            config: configuration.inner.clone(),
            state: State::Idle,
            sig_stop: QResetChannel::new(),
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
        let rx;
        {
            let mut lk = self.ctx.lock().unwrap();
            if lk.state == State::Stopped {
                return None;
            }
            assert_ne!(lk.state, State::Idle);
            rx = lk.ch.pop();
        }
        let res = rx.await;
        match res {
            Ok(c) => Some(c),
            Err(_) => None,
        }
    }

    pub async fn stop(&mut self) {
        let rx;
        {
            let mut lk = self.ctx.lock().unwrap();
            assert_eq!(lk.state, State::Started);
            lk.state = State::StopRequested;
            rx = lk.sig_stop.reset();
        }
        // callback may be invoked in the same thread.
        info!("listner stop requested.");
        self.inner.inner.stop();
        info!("wait for stop_rx signal.");
        // wait for drain.
        rx.await;
        info!("wait for stop_rx signal ok.");
    }
}

impl QListenerCtx {
    fn on_new_connection(&mut self, conn: Handle) {
        let c = QConnection::attach(self._api.clone(), conn, &self.config.inner);
        self.ch.insert(c);
    }

    fn on_stop_complete(&mut self) {
        self.ch.close(0);
        self.sig_stop.set(());
    }
}
