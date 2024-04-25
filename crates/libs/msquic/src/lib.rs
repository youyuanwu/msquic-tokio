use std::sync::Arc;

use c2::Api;

#[cfg(not(test))]
pub use log::info;

use utils::SBox;

#[cfg(test)]
pub use std::println as info;

pub mod buffer;
pub mod config;
pub mod conn;
pub mod listener;
pub mod reg;
pub mod stream;
mod utils;

// Some useful defs
pub const QUIC_STATUS_PENDING: u32 = 0x703e5;
pub const QUIC_STATUS_SUCCESS: u32 = 0;

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
        config::QConfiguration,
        conn::QConnection,
        listener::QListener,
        reg::QRegistration,
        stream::QStream,
        QApi,
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
        let q_reg = QRegistration::new(&api, &config);

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
