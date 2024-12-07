use bytes::Buf;
use http::Uri;
use msquic_sys2::{
    Addr, CertificateHash, CertificateUnion, CredentialConfig, RegistrationConfig, Settings,
    ADDRESS_FAMILY_UNSPEC, CREDENTIAL_FLAG_CLIENT, CREDENTIAL_FLAG_NO_CERTIFICATE_VALIDATION,
    CREDENTIAL_TYPE_CERTIFICATE_HASH, CREDENTIAL_TYPE_NONE, EXECUTION_PROFILE_LOW_LATENCY,
};
use tracing::info;

use crate::core::{
    api::QApi,
    buffer::{QBufferVec, QVecBuffer},
    config::QConfiguration,
    conn::QConnection,
    listener::QListener,
    reg::QRegistration,
};
use crate::msh3::H3Conn;

/// Send a GET request to target server.
async fn send_get_request(uri: Uri) {
    let api = QApi::default();

    let app_name = std::ffi::CString::new("testapp").unwrap();
    let config = RegistrationConfig {
        app_name: app_name.as_ptr(),
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

    info!("client conn open");
    let mut conn = QConnection::open(&q_reg);
    info!("client conn start");
    conn.start(&client_config, uri.host().unwrap(), uri.port_u16().unwrap())
        .await
        .unwrap();

    // tokio::time::sleep(std::time::Duration::from_millis(1)).await;

    let h_conn = H3Conn::new(conn);

    let (mut driver, mut send_request) = h3::client::new(h_conn.clone()).await.unwrap();

    let drive = async move {
        std::future::poll_fn(|cx| driver.poll_close(cx)).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    // tokio::time::sleep(std::time::Duration::from_millis(3)).await;
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
        let mut data = vec![];
        while let Some(mut chunk) = stream.recv_data().await? {
            // let mut out = tokio::io::stdout();
            // tokio::io::AsyncWriteExt::write_all_buf(&mut out, &mut chunk).await?;
            // tokio::io::AsyncWriteExt::flush(&mut out).await?;
            let mut dst = vec![0; chunk.remaining()];
            chunk.copy_to_slice(&mut dst[..]);
            data.extend_from_slice(&dst);
        }
        let body = String::from_utf8_lossy(&data);
        info!("body: {}", body);
        // tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        Ok::<_, Box<dyn std::error::Error>>(())
    };

    let (req_res, drive_res) = tokio::join!(request, drive);
    if let Err(e) = req_res {
        tracing::error!("req_err {e:?}");
    }
    if let Err(e) = drive_res {
        tracing::error!("drive_res {e:?}");
    }
    info!("client ended success");
}

#[test]
fn client_test_apache() {
    crate::tests::try_setup_tracing();
    // This does not work (cloudflare servers):
    // let uri = http::Uri::from_static("https://quic.tech:8443/");
    // let uri = http::Uri::from_static("https://cloudflare-quic.com:443/");
    let uri = http::Uri::from_static("https://docs.trafficserver.apache.org:443/");
    // use tokio
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(send_get_request(uri));
}

#[test]
fn client_test_h2o() {
    crate::tests::try_setup_tracing();
    let uri = http::Uri::from_static("https://h2o.examp1e.net:443");
    // use smol runtime.
    smol::block_on(send_get_request(uri));
}

#[test]
fn basic_server_test() {
    crate::tests::try_setup_tracing();
    info!("Test start");
    let cert_hash = crate::tests::get_test_cert_hash();
    info!("Using cert_hash: [{cert_hash}]");

    let api = QApi::default();
    let app_name = std::ffi::CString::new("testapp").unwrap();
    let config = RegistrationConfig {
        app_name: app_name.as_ptr(),
        execution_profile: EXECUTION_PROFILE_LOW_LATENCY,
    };
    let q_reg = QRegistration::new(&api, &config);

    let args: [QVecBuffer; 1] = ["h3".into()];
    let alpn = QBufferVec::from(args.as_slice());
    let mut settings = Settings::new();
    settings.set_idle_timeout_ms(1000);
    settings.set_peer_bidi_stream_count(1);
    settings.set_server_resumption_level(2); // QUIC_SERVER_RESUME_AND_ZERORTT

    let q_config = QConfiguration::new(&q_reg, alpn.as_buffers(), &settings);
    {
        let mut hash_array: [u8; 20] = [0; 20];
        hex::decode_to_slice(cert_hash.as_bytes(), &mut hash_array).expect("Decoding failed");

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

    let q_req_copy = q_reg.clone();
    let (sht_tx, mut sht_rx) = tokio::sync::oneshot::channel::<()>();
    let th = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            // config needs to be dropped before reg.
            let q_config = q_config;
            let mut l;
            {
                let local_address = Addr::ipv4(ADDRESS_FAMILY_UNSPEC, 4568_u16.to_be(), 0);
                l = QListener::open(&q_req_copy, &q_config);
                info!("Start listener.");
                let alpn = QBufferVec::from(args.as_slice());
                l.start(alpn.as_buffers(), &local_address);
            }
            let mut i = 0;
            loop {
                let conn_id = i;
                info!("server accept conn {}", i);
                i += 1;
                let conn = tokio::select! {
                    val = l.accept() => val,
                    _ = &mut sht_rx => {
                        info!("server accepted interrupted.");
                        None // stop accept and break.
                    }
                };
                if conn.is_none() {
                    info!("server accepted conn end");
                    break;
                }
                // let rth = rt.handle().clone();
                // use another task to handle conn
                rt.spawn(async move {
                    let conn = conn.unwrap();
                    info!("server accepted conn id={}", conn_id);
                    info!("server conn connect");
                    let mut h3_conn: h3::server::Connection<H3Conn, bytes::Bytes> =
                        h3::server::Connection::new(H3Conn::new(conn))
                            .await
                            .unwrap();
                    loop {
                        match h3_conn.accept().await {
                            Ok(Some((req, mut stream))) => {
                                tokio::spawn(async move {
                                    info!("new request: {:#?}", req);
                                    drop(req);
                                    let resp =
                                        http::Response::builder().status(200).body(()).unwrap();

                                    // send headers
                                    match stream.send_response(resp).await {
                                        Ok(_) => {
                                            tracing::info!("successfully respond to connection");
                                        }
                                        Err(err) => {
                                            tracing::error!(
                                                "unable to send response to connection peer: {:?}",
                                                err
                                            );
                                        }
                                    }
                                    // send body
                                    let body = bytes::Bytes::from_static(b"mydata");
                                    match stream.send_data(body).await {
                                        Ok(_) => tracing::info!("send body ok"),
                                        Err(e) => tracing::error!("send body err: {e}"),
                                    }
                                    // tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                                    // close stream. it sends stuff without check ready.
                                    match stream.finish().await {
                                        Ok(_) => {
                                            tracing::info!("close stream ok")
                                        }
                                        Err(e) => tracing::error!("close stream err: {e}"),
                                    }

                                    // TODO: stream drop can happen to quickly.
                                    // tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                                });
                            }

                            // indicating no more streams to be received
                            Ok(None) => {
                                break;
                            }

                            Err(err) => {
                                tracing::error!("error on accept {}", err);
                                match err.get_error_level() {
                                    h3::error::ErrorLevel::ConnectionError => break,
                                    h3::error::ErrorLevel::StreamError => continue,
                                }
                            }
                        }
                    }
                });
            }
            info!("server listener stop");
            l.stop().await;
            info!("server listner stop finish");
        });
        info!("tokio server end.");
    });

    // std::thread::sleep(Duration::from_secs(100));
    // send request
    let uri = http::Uri::from_static("https://localhost:4568");
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(send_get_request(uri));
    //std::thread::sleep(Duration::from_secs(1));
    sht_tx.send(()).unwrap();
    th.join().unwrap();
}
