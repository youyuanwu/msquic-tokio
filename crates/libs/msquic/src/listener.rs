use std::{ffi::c_void, sync::Arc};

use crate::{conn::QConnection, info};
use c2::{
    Addr, Buffer, Configuration, Handle, Listener, ListenerEvent, LISTENER_EVENT_NEW_CONNECTION,
};
use tokio::sync::mpsc;

use crate::{config::QConfiguration, reg::QRegistration, utils::SBox, QApi};

pub struct QListener {
    _api: QApi,
    inner: SBox<Listener>,
    ctx: Box<QListenerCtx>,
}

struct QListenerCtx {
    _api: QApi,
    tx: mpsc::Sender<QConnection>,
    rx: mpsc::Receiver<QConnection>,
    config: Arc<SBox<Configuration>>,
}

extern "C" fn listener_handler(
    listener: Handle,
    context: *mut c_void,
    event: &ListenerEvent,
) -> u32 {
    assert!(!context.is_null());
    let ctx = unsafe { (context as *mut QListenerCtx).as_ref().unwrap() };
    let status = 0;

    match event.event_type {
        LISTENER_EVENT_NEW_CONNECTION => {
            let raw = unsafe { event.payload.new_connection };
            let h = raw.connection as Handle;
            info!(
                "[{:?}] LISTENER_EVENT_NEW_CONNECTION conn=[{:?}]",
                listener, h
            );
            let c = QConnection::attach(ctx._api.clone(), h, &ctx.config.inner);
            let info = unsafe { raw.info.as_ref().unwrap() };
            info!(
                "[{:?}] LISTENER_EVENT_NEW_CONNECTION conn=[{:?}] info={:?}",
                listener, h, info
            );
            ctx.tx.blocking_send(c).unwrap();
        }
        _ => {
            // stop complete
            info!(
                "[{:?}] LISTENER_EVENT unknown {}",
                listener, event.event_type
            );
        }
    };

    status
}

impl QListener {
    // server open listener
    pub fn open(registration: &QRegistration, configuration: &QConfiguration) -> Self {
        let (tx, rx) = mpsc::channel(2);
        let context = Box::new(QListenerCtx {
            _api: registration.api.clone(),
            tx,
            rx,
            config: configuration.inner.clone(),
        });
        let l = Listener::new(
            &registration.inner.inner,
            listener_handler,
            (&*context) as *const QListenerCtx as *const c_void,
        );
        Self {
            _api: registration.api.clone(),
            inner: SBox { inner: l },
            ctx: context,
        }
    }

    pub fn start(&self, alpn: &[Buffer], local_address: &Addr) {
        self.inner.inner.start(alpn, local_address)
    }

    pub async fn accept(&mut self) -> Option<QConnection> {
        self.ctx.rx.recv().await
    }
}
