use crate::{config::QConfiguration, info, reg::QRegistration, stream::QStream};
use std::{
    ffi::c_void,
    io::{Error, ErrorKind},
};

use c2::{
    Configuration, Connection, ConnectionEvent, Handle, SendResumptionFlags,
    CONNECTION_EVENT_CONNECTED, CONNECTION_EVENT_PEER_STREAM_STARTED, CONNECTION_EVENT_RESUMED,
    CONNECTION_EVENT_RESUMPTION_TICKET_RECEIVED, CONNECTION_EVENT_SHUTDOWN_COMPLETE,
    CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER, CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT,
};
use tokio::sync::{mpsc, oneshot};

use crate::{utils::SBox, QApi};

pub struct QConnection {
    pub _api: QApi,
    pub inner: SBox<Connection>,
    ctx: Box<QConnectionCtx>,
}

pub struct QConnectionCtx {
    _api: QApi,
    strm_tx: mpsc::Sender<QStream>,
    strm_rx: mpsc::Receiver<QStream>,
    conn_tx: Option<oneshot::Sender<()>>,
    conn_rx: Option<oneshot::Receiver<()>>,
    shtdwn_tx: Option<oneshot::Sender<()>>,
    shtdwn_rx: Option<oneshot::Receiver<()>>,
}

extern "C" fn qconnection_callback_handler(
    connection: Handle,
    context: *mut c_void,
    event: &ConnectionEvent,
) -> u32 {
    assert!(!context.is_null());
    let ctx = unsafe { (context as *mut QConnectionCtx).as_mut().unwrap() };
    let status = 0;

    match event.event_type {
        CONNECTION_EVENT_CONNECTED => {
            info!("[{:?}] CONNECTION_EVENT_CONNECTED", connection);
            // server xor client connected
            ctx.conn_tx.take().unwrap().send(()).unwrap();
        }
        CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT => {
            let raw = unsafe { event.payload.shutdown_initiated_by_transport };
            info!(
                "[{:?}] CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT ec=0x{:x} status=0x{:x}",
                connection, raw.error_code, raw.status
            );
            assert!(ctx.shtdwn_tx.is_some());
        }
        CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER => {
            info!(
                "[{:?}] CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER",
                connection,
            );
            assert!(ctx.shtdwn_tx.is_some());
        }
        CONNECTION_EVENT_SHUTDOWN_COMPLETE => {
            info!("[{:?}] CONNECTION_EVENT_SHUTDOWN_COMPLETE", connection,);
            assert!(ctx.shtdwn_tx.is_some());
            // TODO: close stream channel
            if let Some(tx) = ctx.shtdwn_tx.take() {
                tx.send(()).unwrap();
            }
        }
        CONNECTION_EVENT_PEER_STREAM_STARTED => {
            assert!(ctx.shtdwn_tx.is_some());
            // stream is accepted on server
            let raw = unsafe { event.payload.peer_stream_started };
            let h = raw.stream as Handle;
            info!(
                "[{:?}] CONNECTION_EVENT_PEER_STREAM_STARTED stream=[{:?}]",
                connection, h
            );
            let s = QStream::attach(ctx._api.clone(), h);
            ctx.strm_tx.blocking_send(s).unwrap();
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
    fn new(api: &QApi) -> Self {
        let (strm_tx, strm_rx) = mpsc::channel(2);
        let (conn_tx, conn_rx) = oneshot::channel();
        let (shdwn_tx, shdwn_rx) = oneshot::channel();
        Self {
            _api: api.clone(),
            strm_tx,
            strm_rx,
            conn_tx: Some(conn_tx),
            conn_rx: Some(conn_rx),
            shtdwn_tx: Some(shdwn_tx),
            shtdwn_rx: Some(shdwn_rx),
        }
    }
}

impl QConnection {
    // this is for attaching accepted connections
    pub fn attach(api: QApi, h: Handle, config: &Configuration) -> Self {
        let c = Connection::from_parts(h, &api.inner.inner);
        let context = Box::new(QConnectionCtx::new(&api));

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
        let context = Box::new(QConnectionCtx::new(&registration.api));
        c.open(
            &registration.inner.inner,
            qconnection_callback_handler,
            (&*context) as *const QConnectionCtx as *const c_void,
        );
        //info!("[{:#?}] Connection front end open", c);
        Self::new(c, registration.api.clone(), context)
    }

    // wait for connect callback.
    pub async fn connect(&mut self) {
        self.ctx.conn_rx.take().unwrap().await.unwrap()
    }

    pub fn send_resumption_ticket(&self, flags: SendResumptionFlags) {
        self.inner.inner.send_resumption_ticket(flags)
    }

    // accept stream
    pub async fn accept(&mut self) -> Option<QStream> {
        self.ctx.strm_rx.recv().await
    }

    pub async fn start(
        &mut self,
        configuration: &QConfiguration,
        server_name: &str,
        server_port: u16,
    ) -> Result<(), Error> {
        self.inner
            .inner
            .start(&configuration.inner.inner, server_name, server_port);
        self.ctx
            .conn_rx
            .take()
            .unwrap()
            .await
            .map_or(Err(Error::from(ErrorKind::NotConnected)), Ok)?;
        Ok(())
    }
}
