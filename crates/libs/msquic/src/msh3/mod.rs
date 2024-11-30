// h3 wrappings for msquic

use std::{fmt::Display, sync::Arc, task::Poll};

use bytes::{Buf, BytesMut};
use c2::{
    SEND_FLAG_NONE, STREAM_OPEN_FLAG_NONE, STREAM_OPEN_FLAG_UNIDIRECTIONAL,
    STREAM_SHUTDOWN_FLAG_GRACEFUL,
};
use h3::quic::{BidiStream, Connection, OpenStreams, RecvStream, SendStream};
use tracing::info;

use crate::{conn::QConnection, stream::QStream};

#[derive(Debug)]
pub struct H3Error {
    status: std::io::Error,
    error_code: Option<u64>,
}

impl H3Error {
    pub fn new(status: std::io::Error, ec: Option<u64>) -> Self {
        Self {
            status,
            error_code: ec,
        }
    }
}

impl h3::quic::Error for H3Error {
    fn is_timeout(&self) -> bool {
        self.status.kind() == std::io::ErrorKind::TimedOut
    }

    fn err_code(&self) -> Option<u64> {
        self.error_code
    }
}

impl std::error::Error for H3Error {}

impl Display for H3Error {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

#[derive(Clone)]
pub struct H3Conn {
    inner: Arc<std::sync::Mutex<QConnection>>, // some functions require mut. we need clone. TODO: move mutex to inner.
}

impl H3Conn {
    pub fn new(inner: QConnection) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(inner)),
        }
    }
}

impl<B: Buf> OpenStreams<B> for H3Conn {
    type BidiStream = H3Stream;

    type SendStream = H3Stream;

    type OpenError = H3Error;

    fn poll_open_bidi(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::BidiStream, Self::OpenError>> {
        info!("msh3 conn poll_open_bidi");
        let lk = self.inner.as_ref().lock().unwrap();
        let s = QStream::open(&lk, STREAM_OPEN_FLAG_NONE);
        s.start_only(SEND_FLAG_NONE);
        std::thread::sleep(std::time::Duration::from_millis(1));
        // TODO: start? maybe sleep abit for now?
        Poll::Ready(Ok(H3Stream::new(s)))
    }

    fn poll_open_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Self::SendStream, Self::OpenError>> {
        info!("msh3 conn poll_open_send");
        let lk = self.inner.as_ref().lock().unwrap();
        let s = QStream::open(&lk, STREAM_OPEN_FLAG_UNIDIRECTIONAL);
        s.start_only(STREAM_OPEN_FLAG_UNIDIRECTIONAL);
        std::thread::sleep(std::time::Duration::from_millis(1));
        // TODO: start? maybe sleep abit for now?
        Poll::Ready(Ok(H3Stream::new(s)))
    }

    fn close(&mut self, code: h3::error::Code, _reason: &[u8]) {
        info!("msh3 conn close");
        let lk = self.inner.as_ref().lock().unwrap();
        // TODO?
        lk.shutdown_only(code.value())
    }
}

impl<B: Buf> Connection<B> for H3Conn {
    type RecvStream = H3Stream;

    type OpenStreams = H3Conn;

    type AcceptError = H3Error;

    fn poll_accept_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::RecvStream>, Self::AcceptError>> {
        let mut lk = self.inner.as_ref().lock().unwrap();
        // TODO: propogate error.
        let res = lk.poll_accept_uni(cx).map(|x| Ok(x.map(H3Stream::new)));
        info!("msh3 conn poll_accept_recv: {res:?}");
        res
    }

    fn poll_accept_bidi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::BidiStream>, Self::AcceptError>> {
        let mut lk = self.inner.as_ref().lock().unwrap();
        // TODO: propogate error.
        let res = lk.poll_accept(cx).map(|x| Ok(x.map(H3Stream::new)));
        info!("msh3 conn poll_accept_bidi {res:?}");
        res
    }

    fn opener(&self) -> Self::OpenStreams {
        info!("msh3 conn opener");
        self.clone()
    }
}

#[derive(Debug)]
pub struct H3Stream {
    inner: QStream,
    shutdown: bool,
}

impl H3Stream {
    fn new(s: QStream) -> Self {
        Self {
            inner: s,
            shutdown: false,
        }
    }

    fn get_id(&self) -> h3::quic::StreamId {
        self.inner.get_id().try_into().expect("cannot convert id")
    }
}

impl<B: Buf> SendStream<B> for H3Stream {
    type Error = H3Error;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let res = self
            .inner
            .poll_ready_send(cx)
            .map_err(|e| H3Error::new(e, None));
        info!("msh3 stream [{}] poll_ready {res:?}", self.get_id());
        res
    }

    fn send_data<T: Into<h3::quic::WriteBuf<B>>>(&mut self, data: T) -> Result<(), Self::Error> {
        info!("msh3 stream [{}] send_data ok", self.get_id());
        let b: h3::quic::WriteBuf<B> = data.into();
        self.inner.send_only(b, SEND_FLAG_NONE);
        Ok(())
    }

    // send shutdown signal to peer?
    fn poll_finish(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // close the stream
        let do_work = !self.shutdown;
        if do_work {
            self.inner.shutdown_only(STREAM_SHUTDOWN_FLAG_GRACEFUL);
            self.shutdown = true;
        }
        // Does not need to poll? Seems like quinn does not poll this.
        // let res = self.inner.poll_shutdown(cx).map_err(|e| H3Error::new(e, None));
        info!(
            "msh3 stream [{}] poll_finish do work ok: {do_work}",
            self.get_id()
        );
        Poll::Ready(Ok(()))
    }

    fn reset(&mut self, _reset_code: u64) {
        panic!("reset not supported")
    }

    fn send_id(&self) -> h3::quic::StreamId {
        self.get_id()
    }
}

impl RecvStream for H3Stream {
    type Buf = BytesMut;

    type Error = H3Error;

    // currently error is not propagated.
    fn poll_data(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Option<Self::Buf>, Self::Error>> {
        let res = match self.inner.poll_receive(cx) {
            std::task::Poll::Ready(br) => Poll::Ready(Ok(br)),
            std::task::Poll::Pending => Poll::Pending,
        };
        info!("msh3 stream [{}] poll_data {res:?}", self.get_id());
        res
    }

    fn stop_sending(&mut self, error_code: u64) {
        info!("msh3 stream [{}] stop_sending ok", self.get_id());
        self.inner.stop_sending(error_code);
    }

    fn recv_id(&self) -> h3::quic::StreamId {
        self.get_id()
    }
}

impl<B: Buf> BidiStream<B> for H3Stream {
    type SendStream = H3Stream;

    type RecvStream = H3Stream;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        let cp = self.inner.clone();
        (self, H3Stream::new(cp))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use c2::{
        CredentialConfig, RegistrationConfig, Settings, CREDENTIAL_FLAG_CLIENT,
        CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION, CREDENTIAL_TYPE_NONE,
        EXECUTION_PROFILE_LOW_LATENCY,
    };
    use tracing::info;

    use crate::{
        buffer::{QBufferVec, QVecBuffer},
        config::QConfiguration,
        conn::QConnection,
        msh3::H3Conn,
        reg::QRegistration,
        QApi,
    };

    #[test]
    fn basic_test() {
        let _ = tracing_subscriber::fmt().try_init();
        info!("Test start");
        let cert_hash = crate::tests::get_test_cert_hash();
        info!("Using cert_hash: [{cert_hash}]");

        let api = QApi::default();

        let config = RegistrationConfig {
            app_name: "testapp".as_ptr() as *const i8,
            execution_profile: EXECUTION_PROFILE_LOW_LATENCY,
        };
        let q_reg = QRegistration::new(&api, &config);

        let args: [QVecBuffer; 1] = ["h3".into()];
        let alpn = QBufferVec::from(args.as_slice());

        // create an client
        // open client
        let mut client_settings = Settings::new();
        client_settings.set_idle_timeout_ms(2000);
        let client_config = QConfiguration::new(&q_reg, alpn.as_buffers(), &client_settings);
        {
            let mut cred_config = CredentialConfig::new_client();
            cred_config.cred_type = CREDENTIAL_TYPE_NONE;
            cred_config.cred_flags = CREDENTIAL_FLAG_CLIENT;
            cred_config.cred_flags |= CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION;
            client_config.load_cred(&cred_config);
        }

        let uri = http::Uri::from_static("https://h2o.examp1e.net:443");

        // run client in another runtime.
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async move {
                let client_config = client_config;
                tokio::time::sleep(Duration::from_secs(1)).await;
                info!("client conn open");
                let mut conn = QConnection::open(&q_reg);
                info!("client conn start");
                conn.start(&client_config, uri.host().unwrap(), uri.port_u16().unwrap())
                    .await
                    .unwrap();

                tokio::time::sleep(std::time::Duration::from_millis(1)).await;

                let h_conn = H3Conn::new(conn);

                let (mut driver, mut send_request) = h3::client::new(h_conn.clone()).await.unwrap();

                let drive = async move {
                    std::future::poll_fn(|cx| driver.poll_close(cx)).await?;
                    Ok::<(), Box<dyn std::error::Error>>(())
                };

                tokio::time::sleep(std::time::Duration::from_millis(3)).await;
                // In the following block, we want to take ownership of `send_request`:
                // the connection will be closed only when all `SendRequest`s instances
                // are dropped.
                //
                //             So we "move" it.
                //                  vvvv
                let request = async move {
                    info!("sending request ...");

                    let req = http::Request::builder().uri(uri).body(())?;

                    // sending request results in a bidirectional stream,
                    // which is also used for receiving response
                    let mut stream = send_request.send_request(req).await?;

                    // finish on the sending side
                    stream.finish().await?;

                    info!("receiving response ...");

                    let resp = stream.recv_response().await?;

                    info!("response: {:?} {}", resp.version(), resp.status());
                    info!("headers: {:#?}", resp.headers());

                    // `recv_data()` must be called after `recv_response()` for
                    // receiving potential response body
                    while let Some(mut chunk) = stream.recv_data().await? {
                        let mut out = tokio::io::stdout();
                        tokio::io::AsyncWriteExt::write_all_buf(&mut out, &mut chunk).await?;
                        tokio::io::AsyncWriteExt::flush(&mut out).await?;
                    }

                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    Ok::<_, Box<dyn std::error::Error>>(())
                };

                let (req_res, drive_res) = tokio::join!(request, drive);
                if let Err(e) = req_res {
                    tracing::error!("req_err {e:?}");
                }
                if let Err(e) = drive_res {
                    tracing::error!("drive_res {e:?}");
                }

                // wait for the connection to be closed before exiting
                // h_conn.inner.lock().unwrap().
                info!("sleeping at the end");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            });
    }
}
