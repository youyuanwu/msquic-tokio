use crate::{
    config::QConfiguration,
    info,
    reg::QRegistration,
    stream::QStream,
    sync::{QReceiver, QResetChannel, QWakableSig},
};
use std::{ffi::c_void, fmt::Debug, future::poll_fn, io::Error, sync::Mutex, task::Poll};

use c2::{
    Configuration, Connection, ConnectionEvent, Handle, SendResumptionFlags,
    CONNECTION_EVENT_CONNECTED, CONNECTION_EVENT_PEER_STREAM_STARTED, CONNECTION_EVENT_RESUMED,
    CONNECTION_EVENT_RESUMPTION_TICKET_RECEIVED, CONNECTION_EVENT_SHUTDOWN_COMPLETE,
    CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER, CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT,
    CONNECTION_SHUTDOWN_FLAG_NONE, STREAM_OPEN_FLAG_UNIDIRECTIONAL,
};

use crate::{utils::SBox, QApi};

pub struct QConnection {
    pub _api: QApi,
    pub inner: SBox<Connection>,
    ctx: Box<Mutex<QConnectionCtx>>,
    strm_rx: tokio::sync::mpsc::UnboundedReceiver<QStream>,
    strm_uni_rx: tokio::sync::mpsc::UnboundedReceiver<QStream>,
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
    strm_tx: Option<tokio::sync::mpsc::UnboundedSender<QStream>>,
    strm_uni_tx: Option<tokio::sync::mpsc::UnboundedSender<QStream>>,
    shtdwn_sig: QWakableSig<()>,
    //state: Mutex<State>,
    conn_ch: QResetChannel<ConnStatus>, // handle connect success or transport close. corresponds to proceed_rx in server wait case.
    proceed_rx: Option<QReceiver<ConnStatus>>, // used for server wait conn. currently not used.
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
            // server xor client connected.
            // (it seems like server does not need to wait for this)
            ctx.on_connected(None);
        }
        // Not defined. TODO: client needs to fail when this happends.
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
            // let e = windows_core::Error::from_hresult(windows_core::HRESULT::from_win32(raw.error_code ));
            info!(
                "[{:?}] CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER: app ec {}",
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
            let is_uni = (raw.flags & STREAM_OPEN_FLAG_UNIDIRECTIONAL) != 0;
            info!(
                "[{:?}] CONNECTION_EVENT_PEER_STREAM_STARTED stream=[{:?}], is_uni = {is_uni}",
                connection, h
            );
            ctx.on_peer_stream_started(h, is_uni);
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
        // TODO: set the status?
        self.strm_tx = None;
        self.strm_uni_tx = None;
    }
    fn on_shutdown_initiated_by_peer(&mut self, _ec: u64) {
        // peer will no longer send?
        self.strm_uni_tx = None;
    }
    fn on_shutdown_complete(&mut self) {
        // When shutdown triggerend from front end directly this can happen without transport or peer case.
        self.strm_tx = None;
        self.strm_uni_tx = None;
        self.shtdwn_sig.set(());
    }
    fn on_peer_stream_started(&mut self, h: Handle, uni: bool) {
        let s = QStream::attach(self._api.clone(), h);
        let ch = match uni {
            true => self.strm_uni_tx.as_ref().expect("sender none"),
            false => self.strm_tx.as_ref().expect("sender none"),
        };
        ch.send(s).expect("cannot send");
    }
}

impl QConnection {
    // this is for attaching accepted connections
    pub fn attach(api: QApi, h: Handle, config: &Configuration) -> Self {
        let c = Connection::from_parts(h, &api.inner.inner);
        let (strm_tx, strm_rx) = tokio::sync::mpsc::unbounded_channel();
        let (strm_uni_tx, strm_uni_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = QConnectionCtx {
            _api: api.clone(),
            strm_tx: Some(strm_tx),
            strm_uni_tx: Some(strm_uni_tx),
            shtdwn_sig: QWakableSig::default(),
            conn_ch: QResetChannel::new(),
            proceed_rx: None,
        };

        let context = Box::new(Mutex::new(ctx));
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
        Self {
            _api: api,
            inner: SBox::new(c),
            ctx: context,
            strm_rx,
            strm_uni_rx,
        }
    }

    // server proceed, wait for connect event. currently not used.
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

    // open a client
    pub fn open(registration: &QRegistration) -> Self {
        let c = Connection::new(&registration.inner.inner);
        let (strm_tx, strm_rx) = tokio::sync::mpsc::unbounded_channel();
        let (strm_uni_tx, strm_uni_rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = QConnectionCtx {
            _api: registration.api.clone(),
            strm_tx: Some(strm_tx),
            strm_uni_tx: Some(strm_uni_tx),
            shtdwn_sig: QWakableSig::default(),
            conn_ch: QResetChannel::new(),
            proceed_rx: None,
        };
        let context = Box::new(Mutex::new(ctx));
        c.open(
            &registration.inner.inner,
            qconnection_callback_handler,
            (&*context) as *const Mutex<QConnectionCtx> as *const c_void,
        );
        //info!("[{:#?}] Connection front end open", c);
        Self {
            _api: registration.api.clone(),
            inner: SBox::new(c),
            ctx: context,
            strm_rx,
            strm_uni_rx,
        }
    }

    pub fn send_resumption_ticket(&self, flags: SendResumptionFlags) {
        self.inner.inner.send_resumption_ticket(flags)
    }

    // accept stream for server
    pub async fn accept(&mut self) -> Option<QStream> {
        self.strm_rx.recv().await
    }

    pub fn poll_accept(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Option<QStream>> {
        self.strm_rx.poll_recv(cx)
    }

    // accept uni direction stream
    pub async fn accept_uni(&mut self) -> Option<QStream> {
        self.strm_uni_rx.recv().await
    }

    pub fn poll_accept_uni(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Option<QStream>> {
        self.strm_uni_rx.poll_recv(cx)
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

    pub fn shutdown_only(&self, ec: u64) {
        self.inner.inner.shutdown(CONNECTION_SHUTDOWN_FLAG_NONE, ec); // ec
    }

    pub fn poll_shutdown(&mut self, cx: &mut std::task::Context<'_>) -> Poll<()> {
        let mut lk = self.ctx.lock().unwrap();
        let p = lk.shtdwn_sig.poll(cx);
        match p {
            Poll::Ready(_) => Poll::Ready(()),
            Poll::Pending => Poll::Pending,
        }
    }

    pub async fn shutdown(&mut self, ec: u64) {
        {
            let mut lk = self.ctx.lock().unwrap();
            if !lk.shtdwn_sig.is_frontend_pending() {
                lk.shtdwn_sig.set_frontend_pending();
                self.shutdown_only(ec)
            }
        }
        let fu = poll_fn(|cx| self.poll_shutdown(cx));
        fu.await
    }
}
