use crate::{
    config::QConfiguration,
    info,
    reg::QRegistration,
    stream::QStream,
    sync::{QQueue, QReceiver, QResetChannel, QSignal},
};
use std::{ffi::c_void, fmt::Debug, io::Error, sync::Mutex};

use c2::{
    Configuration, Connection, ConnectionEvent, Handle, SendResumptionFlags,
    CONNECTION_EVENT_CONNECTED, CONNECTION_EVENT_PEER_STREAM_STARTED, CONNECTION_EVENT_RESUMED,
    CONNECTION_EVENT_RESUMPTION_TICKET_RECEIVED, CONNECTION_EVENT_SHUTDOWN_COMPLETE,
    CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER, CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT,
    CONNECTION_SHUTDOWN_FLAG_NONE,
};

use crate::{utils::SBox, QApi};

pub struct QConnection {
    pub _api: QApi,
    pub inner: SBox<Connection>,
    ctx: Box<Mutex<QConnectionCtx>>,
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
#[derive(Debug, Clone, PartialEq)]
enum ShutdownError {
    Ok,                    // no error
    Transport((u64, u32)), // ec and status
    Peer(u64),             // ec
    Complete,
}

#[derive(Debug, Clone, PartialEq)]
enum ConnStatus {
    Ok,                    // connection success
    Transport((u64, u32)), // app ec and status
}

struct QConnectionCtx {
    _api: QApi,
    strm_ch: QQueue<QStream>,
    shtdwn_sig: QSignal,
    //state: Mutex<State>,
    conn_ch: QResetChannel<ConnStatus>, // handle connect success or transport close
    proceed_rx: Option<QReceiver<ConnStatus>>, // used for server wait conn
}

extern "C" fn qconnection_callback_handler(
    connection: Handle,
    context: *mut c_void,
    event: &ConnectionEvent,
) -> u32 {
    assert!(!context.is_null());
    let ctx_mtx = unsafe { (context as *mut Mutex<QConnectionCtx>).as_mut().unwrap() };
    #[allow(clippy::mut_mutex_lock)]
    let mut ctx = ctx_mtx.lock().unwrap();
    let status = 0;
    match event.event_type {
        CONNECTION_EVENT_CONNECTED => {
            info!("[{:?}] CONNECTION_EVENT_CONNECTED", connection);
            // server xor client connected
            ctx.on_connected(None);
        }
        // Not defined
        // CONNECTION_EVENT_CLOSED => {
        // }
        CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT => {
            let raw = unsafe { event.payload.shutdown_initiated_by_transport };
            info!(
                "[{:?}] CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT ec=0x{:x} status=0x{:x}",
                connection, raw.error_code, raw.status
            );
            ctx.on_shutdown_initiated_by_transport(raw.error_code, raw.status);
        }
        // Peer application called connection shutdown.
        // Error code is application defined.
        CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER => {
            let raw = unsafe { event.payload.shutdown_initiated_by_peer };
            info!(
                "[{:?}] CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER: ec {}",
                connection, raw.error_code,
            );
            ctx.on_shutdown_initiated_by_peer(raw.error_code);
        }
        // only invoked after user calls connection shutdown.
        // This can happend without peer or transport.
        CONNECTION_EVENT_SHUTDOWN_COMPLETE => {
            info!("[{:?}] CONNECTION_EVENT_SHUTDOWN_COMPLETE", connection,);
            ctx.on_shutdown_complete();
        }
        CONNECTION_EVENT_PEER_STREAM_STARTED => {
            // stream is accepted on server
            let raw = unsafe { event.payload.peer_stream_started };
            let h = raw.stream as Handle;
            info!(
                "[{:?}] CONNECTION_EVENT_PEER_STREAM_STARTED stream=[{:?}]",
                connection, h
            );
            ctx.on_peer_stream_started(h);
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
        Self {
            _api: api.clone(),
            strm_ch: QQueue::new(),
            shtdwn_sig: QSignal::new(),
            conn_ch: QResetChannel::new(),
            proceed_rx: None,
        }
    }

    // prepare connected event
    fn prepare_connect(&mut self) -> QReceiver<ConnStatus> {
        self.conn_ch.reset()
    }

    fn on_connected(&mut self, ec: Option<(u64, u32)>) {
        let status = match ec {
            Some((ec, status)) => ConnStatus::Transport((ec, status)),
            None => ConnStatus::Ok,
        };
        self.conn_ch.set(status)
    }

    fn on_shutdown_initiated_by_transport(&mut self, ec: u64, status: u32) {
        if self.conn_ch.can_set() {
            self.conn_ch.set(ConnStatus::Transport((ec, status)));
        }
        self.strm_ch.close(status);
    }
    fn on_shutdown_initiated_by_peer(&mut self, _ec: u64) {
        self.strm_ch.close(0);
    }
    fn on_shutdown_complete(&mut self) {
        self.strm_ch.close(0);
        if self.shtdwn_sig.can_set() {
            self.shtdwn_sig.set(());
        }
    }
    fn on_peer_stream_started(&mut self, h: Handle) {
        let s = QStream::attach(self._api.clone(), h);
        self.strm_ch.insert(s);
    }
}

impl QConnection {
    // this is for attaching accepted connections
    pub fn attach(api: QApi, h: Handle, config: &Configuration) -> Self {
        let c = Connection::from_parts(h, &api.inner.inner);
        let context = Box::new(Mutex::new(QConnectionCtx::new(&api)));
        // callback set must happen before the listner callback returns.
        c.set_callback_handler(
            qconnection_callback_handler,
            (&*context) as *const Mutex<QConnectionCtx> as *const c_void,
        );
        // set config
        c.set_configuration(config);

        {
            // init the proceed channel
            let mut ctx = context.lock().unwrap();
            let rx = ctx.prepare_connect();
            ctx.proceed_rx = Some(rx);
        }
        Self::new(c, api, context)
    }

    // server proceed, wait for connect event
    pub async fn proceed(&mut self) -> Result<(), Error> {
        let rx;
        {
            rx = self.ctx.lock().unwrap().proceed_rx.take().unwrap();
        }
        let status = rx.await;
        match status {
            ConnStatus::Ok => Ok(()),
            ConnStatus::Transport((_, s)) => Err(Error::from_raw_os_error(s as i32)),
        }
    }

    fn new(c: Connection, api: QApi, ctx: Box<Mutex<QConnectionCtx>>) -> Self {
        Self {
            _api: api,
            inner: SBox::new(c),
            ctx,
        }
    }

    // open a client
    pub fn open(registration: &QRegistration) -> Self {
        let c = Connection::new(&registration.inner.inner);
        let context = Box::new(Mutex::new(QConnectionCtx::new(&registration.api)));
        c.open(
            &registration.inner.inner,
            qconnection_callback_handler,
            (&*context) as *const Mutex<QConnectionCtx> as *const c_void,
        );
        //info!("[{:#?}] Connection front end open", c);
        Self::new(c, registration.api.clone(), context)
    }

    pub fn send_resumption_ticket(&self, flags: SendResumptionFlags) {
        self.inner.inner.send_resumption_ticket(flags)
    }

    // accept stream
    pub async fn accept(&mut self) -> Option<QStream> {
        let rx;
        {
            rx = self.ctx.lock().unwrap().strm_ch.pop();
        }
        let res = rx.await;
        match res {
            Ok(s) => Some(s),
            Err(_) => None,
        }
    }

    // client start stream
    pub async fn start(
        &mut self,
        configuration: &QConfiguration,
        server_name: &str,
        server_port: u16,
    ) -> Result<(), Error> {
        let rx;
        {
            rx = self.ctx.lock().unwrap().prepare_connect();
            self.inner
                .inner
                .start(&configuration.inner.inner, server_name, server_port);
        }
        let status = rx.await;
        match status {
            ConnStatus::Ok => Ok(()),
            ConnStatus::Transport((_, s)) => Err(Error::from_raw_os_error(s as i32)),
        }
    }

    pub async fn shutdown(&mut self) {
        let rx;
        {
            rx = self.ctx.lock().unwrap().shtdwn_sig.reset();
        }
        info!("conn invoke shutdown");
        // callback maybe sync
        self.inner.inner.shutdown(CONNECTION_SHUTDOWN_FLAG_NONE, 0); // ec
        info!("conn wait for shutdown evnet");
        rx.await;
        info!("conn wait for shutdown evnet end");
    }
}
