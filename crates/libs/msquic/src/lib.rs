use core::slice;
use std::{
    ffi::c_void,
    io::{Error, ErrorKind},
    sync::Arc,
};

use buffer::{QBuffRef, QBufferVec, QVecBuffer, SBuffer};
use c2::{
    Addr, Api, Buffer, Configuration, Connection, ConnectionEvent, CredentialConfig, Handle,
    Listener, ListenerEvent, Registration, RegistrationConfig, SendFlags, SendResumptionFlags,
    Settings, Stream, StreamEvent, StreamOpenFlags, StreamStartFlags, CONNECTION_EVENT_CONNECTED,
    CONNECTION_EVENT_PEER_STREAM_STARTED, CONNECTION_EVENT_RESUMED,
    CONNECTION_EVENT_RESUMPTION_TICKET_RECEIVED, CONNECTION_EVENT_SHUTDOWN_COMPLETE,
    CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_PEER, CONNECTION_EVENT_SHUTDOWN_INITIATED_BY_TRANSPORT,
    LISTENER_EVENT_NEW_CONNECTION, STREAM_EVENT_PEER_RECEIVE_ABORTED,
    STREAM_EVENT_PEER_SEND_ABORTED, STREAM_EVENT_PEER_SEND_SHUTDOWN, STREAM_EVENT_RECEIVE,
    STREAM_EVENT_SEND_COMPLETE, STREAM_EVENT_SEND_SHUTDOWN_COMPLETE,
    STREAM_EVENT_SHUTDOWN_COMPLETE, STREAM_EVENT_START_COMPLETE,
};
#[cfg(not(test))]
use log::info;
use tokio::sync::{
    mpsc::{self},
    oneshot,
};

#[cfg(test)]
use std::println as info;

pub mod buffer;

// Some useful defs
pub const QUIC_STATUS_PENDING: u32 = 0x703e5;
pub const QUIC_STATUS_SUCCESS: u32 = 0;

// sync and send wrapper
struct SBox<T> {
    inner: T,
}

impl<T> SBox<T> {
    fn new(inner: T) -> Self {
        Self { inner }
    }
}

unsafe impl<T> Send for SBox<T> {}
unsafe impl<T> Sync for SBox<T> {}

impl<T> From<T> for SBox<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[derive(Clone)]
pub struct QApi {
    inner: Arc<SBox<Api>>,
}

impl Default for QApi {
    fn default() -> Self {
        Self {
            inner: Arc::new(Api::new().into()),
        }
    }
}

impl QApi {
    pub fn open_registration(&self, config: &RegistrationConfig) -> QRegistration {
        QRegistration::new(self, config)
    }

    //pub fn open_configuration()
}

pub struct QRegistration {
    api: QApi,
    inner: SBox<Registration>,
}

impl QRegistration {
    fn new(api: &QApi, config: &RegistrationConfig) -> Self {
        let inner = Registration::new(&api.inner.inner, config);
        Self {
            api: api.clone(),
            inner: inner.into(),
        }
    }
}

pub struct QConfiguration {
    _api: QApi,
    inner: Arc<SBox<Configuration>>,
}

impl QConfiguration {
    pub fn new(reg: &QRegistration, alpn: &[Buffer], settings: &Settings) -> Self {
        let c = Configuration::new(&reg.inner.inner, alpn, settings);
        Self {
            _api: reg.api.clone(),
            inner: Arc::new(SBox::new(c)),
        }
    }
    pub fn load_cred(&self, cred_config: &CredentialConfig) {
        self.inner.inner.load_credential(cred_config);
    }
}

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

pub struct QConnection {
    _api: QApi,
    inner: SBox<Connection>,
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
    fn attach(api: QApi, h: Handle, config: &Configuration) -> Self {
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
    fn attach(api: QApi, h: Handle) -> Self {
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

#[cfg(test)]
mod tests {

    use std::{thread, time::Duration};

    use c2::{
        Addr, CertificateHash, CertificateUnion, CredentialConfig, RegistrationConfig, Settings,
        ADDRESS_FAMILY_UNSPEC, CREDENTIAL_FLAG_CLIENT, CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION,
        CREDENTIAL_TYPE_CERTIFICATE_HASH, CREDENTIAL_TYPE_NONE, EXECUTION_PROFILE_LOW_LATENCY,
        SEND_FLAG_FIN, SEND_RESUMPTION_FLAG_NONE, STREAM_OPEN_FLAG_NONE, STREAM_START_FLAG_NONE,
    };
    use tokio::sync::oneshot;

    use crate::{
        buffer::{QBufferVec, QVecBuffer},
        QApi, QConfiguration, QConnection, QListener, QStream,
    };

    #[cfg(test)]
    use std::println as info;

    #[test]
    fn basic_test() {
        let _ = env_logger::try_init();
        info!("Test start");
        let api = QApi::default();

        let config = RegistrationConfig {
            app_name: "testapp".as_ptr() as *const i8,
            execution_profile: EXECUTION_PROFILE_LOW_LATENCY,
        };
        let q_reg = api.open_registration(&config);

        let args: [QVecBuffer; 1] = ["sample".into()];
        let alpn = QBufferVec::from(args.as_slice());
        let mut settings = Settings::new();
        settings.set_idle_timeout_ms(1000);
        settings.set_peer_bidi_stream_count(1);
        settings.set_server_resumption_level(2); // QUIC_SERVER_RESUME_AND_ZERORTT

        let q_config = QConfiguration::new(&q_reg, alpn.as_buffers(), &settings);
        {
            let hash = "80CAB3DEDAF26E58F298ACB821282415378E6D65";
            let mut hash_array: [u8; 20] = [0; 20];
            hex::decode_to_slice(hash, &mut hash_array).expect("Decoding failed");

            let mut cred_config = CredentialConfig::new_client();
            cred_config.cred_type = CREDENTIAL_TYPE_CERTIFICATE_HASH;
            cred_config.cred_flags = CREDENTIAL_TYPE_NONE;
            cred_config.certificate = CertificateUnion {
                hash: &CertificateHash {
                    sha_hash: hash_array,
                },
            };

            q_config.load_cred(&cred_config);
        }

        let mut l;

        let local_address = Addr::ipv4(ADDRESS_FAMILY_UNSPEC, 4567_u16.to_be(), 0);
        l = QListener::open(&q_reg, &q_config);
        info!("Start listener.");
        l.start(alpn.as_buffers(), &local_address);

        //let rth = rt.handle().clone();
        let (sht_tx, sht_rx) = oneshot::channel::<()>();
        let th = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut i = 0;
                loop {
                    let conn_id = i;
                    info!("server accept conn {}", i);
                    i += 1;
                    let conn = l.accept().await;
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                    if conn.is_none() {
                        info!("server accepted conn end");
                        break;
                    }
                    // use another task to handle conn
                    rt.spawn(async move {
                        let mut conn = conn.unwrap();
                        info!("server accepted conn id={}", conn_id);
                        info!("server conn connect");
                        conn.connect().await;
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        conn.send_resumption_ticket(SEND_RESUMPTION_FLAG_NONE);
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        loop {
                            info!("server conn accept");
                            let s = conn.accept().await;
                            if s.is_none() {
                                info!("server accept stream end");
                                break;
                            }
                            info!("server accepted stream");
                            let mut s = s.unwrap();
                            let mut buff = [0_u8; 99];
                            let read = s.receive(buff.as_mut_slice()).await.unwrap();
                            assert_eq!(read as usize, "world".len());
                            let args: [QVecBuffer; 1] = [QVecBuffer::from("hello world")];
                            s.send(args.as_slice(), SEND_FLAG_FIN).await.unwrap();
                        }
                    });
                    break; // only accept for 1 request
                }

                sht_rx.await.unwrap();
                info!("tokio shutdown");
            })
        });

        thread::sleep(Duration::from_secs(1));

        // open client
        let mut client_settings = Settings::new();
        client_settings.set_idle_timeout_ms(1000);
        let client_config = QConfiguration::new(&q_reg, alpn.as_buffers(), &client_settings);
        {
            let mut cred_config = CredentialConfig::new_client();
            cred_config.cred_type = CREDENTIAL_TYPE_NONE;
            cred_config.cred_flags = CREDENTIAL_FLAG_CLIENT;
            cred_config.cred_flags |= CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION;
            client_config.load_cred(&cred_config);
        }
        // run client in another runtime.
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async move {
                tokio::time::sleep(Duration::from_secs(1)).await;
                info!("client conn open");
                let mut conn = QConnection::open(&q_reg);
                info!("client conn start");
                conn.start(&client_config, "localhost", 4567).await.unwrap();

                // thread::sleep(Duration::from_secs(5));
                //
                info!("client stream open");
                let mut st = QStream::open(&conn, STREAM_OPEN_FLAG_NONE);
                info!("client stream start");
                st.start(STREAM_START_FLAG_NONE).await;
                let args: [QVecBuffer; 1] = [QVecBuffer::from("hello")];
                info!("client stream send");
                st.send(args.as_slice(), SEND_FLAG_FIN).await.unwrap();

                tokio::time::sleep(Duration::from_millis(1)).await;
                let mut buff = [0_u8; 99];
                info!("client stream receive");
                let read = st.receive(buff.as_mut_slice()).await.unwrap();
                info!("client stream receive read :{}", read);
                // TODO: read write
                // shutdown server
                sht_tx.send(()).unwrap();
            });

        thread::sleep(Duration::from_secs(5));
        th.join().unwrap();
    }
}
