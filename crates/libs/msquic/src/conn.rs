use crate::{config::QConfiguration, info, reg::QRegistration, stream::QStream};
use std::{
    borrow::BorrowMut,
    ffi::c_void,
    fmt::Debug,
    io::{Error, ErrorKind},
    sync::Mutex,
};

use c2::{
    Configuration, Connection, ConnectionEvent, Handle, SendResumptionFlags,
    CONNECTION_EVENT_CONNECTED, CONNECTION_EVENT_PEER_STREAM_STARTED, CONNECTION_EVENT_RESUMED,
    CONNECTION_EVENT_RESUMPTION_TICKET_RECEIVED, CONNECTION_EVENT_SHUTDOWN_COMPLETE,
    CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER, CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT,
    CONNECTION_SHUTDOWN_FLAG_NONE,
};
use tokio::sync::{mpsc, oneshot};

use crate::{utils::SBox, QApi};

pub struct QConnection {
    pub _api: QApi,
    pub inner: SBox<Connection>,
    ctx: Box<QConnectionCtx>,
}

impl Debug for QConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QConnection")
            .field("_api", &"not displayed")
            .field("inner", &self.inner)
            .field("ctx", &"not displayed")
            .finish()
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
enum ShutdownError {
    Ok,                    // no error
    Transport((u64, u32)), // ec and status
    Peer(u64),             // ec
    Complete,
}

#[allow(dead_code)]
enum StreamPayload {
    Stream(QStream),
    Stop(ShutdownError),
}

#[derive(Debug, PartialEq)]
enum State {
    Idle,
    Frontend,
    Backend,
}

struct QConnectionCtx {
    _api: QApi,
    strm_tx: Option<mpsc::Sender<StreamPayload>>,
    strm_rx: mpsc::Receiver<StreamPayload>,
    conn_tx: Option<oneshot::Sender<ShutdownError>>,
    conn_rx: Option<oneshot::Receiver<ShutdownError>>,
    shtdwn_tx: Option<oneshot::Sender<()>>,
    is_shutdown: bool,
    state: Mutex<State>,
}

extern "C" fn qconnection_callback_handler(
    connection: Handle,
    context: *mut c_void,
    event: &ConnectionEvent,
) -> u32 {
    assert!(!context.is_null());
    let ctx = unsafe { (context as *mut QConnectionCtx).as_mut().unwrap() };
    let status = 0;
    let mut state = ctx.state.lock().unwrap();
    *state = State::Backend;
    match event.event_type {
        CONNECTION_EVENT_CONNECTED => {
            info!("[{:?}] CONNECTION_EVENT_CONNECTED", connection);
            // server xor client connected
            if let Some(tx) = ctx.conn_tx.take() {
                tx.send(ShutdownError::Ok).unwrap();
            }
        }
        CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT => {
            let raw = unsafe { event.payload.shutdown_initiated_by_transport };
            info!(
                "[{:?}] CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT ec=0x{:x} status=0x{:x}",
                connection, raw.error_code, raw.status
            );
            let err = ShutdownError::Transport((raw.error_code, raw.status));
            if let Some(tx) = ctx.conn_tx.take() {
                tx.send(err.clone()).unwrap();
            }
            if let Some(tx) = ctx.strm_tx.take() {
                tx.blocking_send(StreamPayload::Stop(err)).unwrap();
            }
        }
        // Peer application called connection shutdown.
        // Error code is application defined.
        CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER => {
            let raw = unsafe { event.payload.shutdown_initiated_by_peer };
            info!(
                "[{:?}] CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER: ec {}",
                connection, raw.error_code,
            );
            let err = ShutdownError::Peer(raw.error_code);
            if let Some(tx) = ctx.conn_tx.take() {
                tx.send(err.clone()).unwrap();
            }
            if let Some(tx) = ctx.strm_tx.take() {
                tx.blocking_send(StreamPayload::Stop(err)).unwrap();
            }
            ctx.is_shutdown = true;
            if let Some(tx) = ctx.shtdwn_tx.take() {
                tx.send(()).unwrap();
            }
        }
        // only invoked after user calls connection shutdown.
        // This can happend without peer or transport.
        CONNECTION_EVENT_SHUTDOWN_COMPLETE => {
            info!("[{:?}] CONNECTION_EVENT_SHUTDOWN_COMPLETE", connection,);
            let err = ShutdownError::Complete;
            if let Some(tx) = ctx.conn_tx.take() {
                tx.send(err.clone()).unwrap();
            }
            // drop stream
            if let Some(tx) = ctx.strm_tx.take() {
                tx.blocking_send(StreamPayload::Stop(err)).unwrap();
            }
            ctx.is_shutdown = true;
            if let Some(tx) = ctx.shtdwn_tx.take() {
                tx.send(()).unwrap();
            }
        }
        CONNECTION_EVENT_PEER_STREAM_STARTED => {
            // stream is accepted on server
            let raw = unsafe { event.payload.peer_stream_started };
            let h = raw.stream as Handle;
            info!(
                "[{:?}] CONNECTION_EVENT_PEER_STREAM_STARTED stream=[{:?}]",
                connection, h
            );
            let s = QStream::attach(ctx._api.clone(), h);
            if let Some(tx) = ctx.strm_tx.borrow_mut() {
                tx.blocking_send(StreamPayload::Stream(s)).unwrap();
            }
        }
        CONNECTION_EVENT_RESUMED => {
            info!("[{:?}] CONNECTION_EVENT_RESUMED", connection,);
        }
        CONNECTION_EVENT_RESUMPTION_TICKET_RECEIVED => {
            info!(
                "[{:?}] CONNECTION_EVENT_RESUMPTION_TICKET_RECEIVED",
                connection,
            );
        }
        _ => {
            info!(
                "[{:?}] CONNECTION_EVENT Unknown {}",
                connection, event.event_type
            );
        }
    }

    status
}

impl QConnectionCtx {
    fn new(api: &QApi, state: State) -> Self {
        let (strm_tx, strm_rx) = mpsc::channel(2);
        let (conn_tx, conn_rx) = oneshot::channel();
        Self {
            _api: api.clone(),
            strm_tx: Some(strm_tx),
            strm_rx,
            conn_tx: Some(conn_tx),
            conn_rx: Some(conn_rx),
            shtdwn_tx: None,
            is_shutdown: false,
            state: Mutex::new(state),
        }
    }
}

impl QConnection {
    // this is for attaching accepted connections
    pub fn attach(api: QApi, h: Handle, config: &Configuration) -> Self {
        let c = Connection::from_parts(h, &api.inner.inner);
        let context = Box::new(QConnectionCtx::new(&api, State::Idle));

        c.set_callback_handler(
            qconnection_callback_handler,
            (&*context) as *const QConnectionCtx as *const c_void,
        );
        // set config
        c.set_configuration(config);
        Self::new(c, api, context)
    }

    fn new(c: Connection, api: QApi, ctx: Box<QConnectionCtx>) -> Self {
        Self {
            _api: api,
            inner: SBox::new(c),
            ctx,
        }
    }

    // open a client
    pub fn open(registration: &QRegistration) -> Self {
        let c = Connection::new(&registration.inner.inner);
        let context = Box::new(QConnectionCtx::new(&registration.api, State::Idle));
        c.open(
            &registration.inner.inner,
            qconnection_callback_handler,
            (&*context) as *const QConnectionCtx as *const c_void,
        );
        //info!("[{:#?}] Connection front end open", c);
        Self::new(c, registration.api.clone(), context)
    }

    // server wait for connect callback.
    pub async fn connect(&mut self) -> Result<(), Error> {
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
        }
        let payload = self.ctx.conn_rx.take().unwrap().await.unwrap();
        match payload {
            ShutdownError::Ok => Ok(()),
            ShutdownError::Transport((_, status)) => Err(Error::from_raw_os_error(status as i32)),
            ShutdownError::Peer(_) => Err(Error::from(ErrorKind::ConnectionAborted)),
            ShutdownError::Complete => Err(Error::from(ErrorKind::ConnectionAborted)),
        }
    }

    pub fn send_resumption_ticket(&self, flags: SendResumptionFlags) {
        self.inner.inner.send_resumption_ticket(flags)
    }

    // accept stream
    pub async fn accept(&mut self) -> Option<QStream> {
        let fu;
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            fu = self.ctx.strm_rx.recv();
        }
        let payload = fu.await;
        match payload {
            Some(s) => match s {
                StreamPayload::Stream(s) => Some(s),
                StreamPayload::Stop(_) => None,
            },
            None => None, // channel closed
        }
    }

    // client start stream
    pub async fn start(
        &mut self,
        configuration: &QConfiguration,
        server_name: &str,
        server_port: u16,
    ) -> Result<(), Error> {
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            self.inner
                .inner
                .start(&configuration.inner.inner, server_name, server_port);
        }
        self.ctx
            .conn_rx
            .take()
            .unwrap()
            .await
            .map_or(Err(Error::from(ErrorKind::NotConnected)), Ok)?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let (shdwn_tx, shdwn_rx) = oneshot::channel();
        {
            let mut state = self.ctx.state.lock().unwrap();
            *state = State::Frontend;
            if self.ctx.is_shutdown {
                info!("conn ctx.is_shutdown already");
                return;
            } else {
                assert!(self.ctx.shtdwn_tx.is_none());
                self.ctx.shtdwn_tx.replace(shdwn_tx);
            }
        }
        info!("conn invoke shutdown");
        // callback maybe sync
        self.inner.inner.shutdown(CONNECTION_SHUTDOWN_FLAG_NONE, 0); // ec
        info!("conn wait for shutdown evnet");
        shdwn_rx.await.unwrap();
        info!("conn wait for shutdown evnet end");
    }
}
