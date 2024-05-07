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
    ctx: Box<QListenerCtx>,
}

struct QListenerCtx {
    _api: QApi,
    ch: QQueue<QConnection>,
    config: Arc<SBox<Configuration>>,
    state: Mutex<State>, // cannot use tokio mutex because the callback maybe invoked sync and block tokio thread
    sig_stop: QSignal,
}

// #[derive(Debug)]
// enum Payload {
//     Conn(QConnection),
// }

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
    let ctx = unsafe { (context as *mut QListenerCtx).as_mut().unwrap() };
    let status = 0;
    // take the state to sync with frontend.
    let mut state = ctx.state.lock().unwrap();
    match event.event_type {
        LISTENER_EVENT_NEW_CONNECTION => {
            let raw = unsafe { event.payload.new_connection };
            let h = raw.connection as Handle;
            let c = QConnection::attach(ctx._api.clone(), h, &ctx.config.inner);
            let info = unsafe { raw.info.as_ref().unwrap() };
            info!(
                "[{:?}] LISTENER_EVENT_NEW_CONNECTION conn=[{:?}] info={:?}",
                listener, h, info
            );
            ctx.ch.insert(c);
        }
        QUIC_LISTENER_EVENT_STOP_COMPLETE => {
            info!("[{:?}] QUIC_LISTENER_EVENT_STOP_COMPLETE", listener);
            assert_eq!(*state, State::StopRequested);
            *state = State::Stopped;
            // stop the acceptor
            ctx.ch.close(0);
            // stop the stop action
            info!("[{:?}] QUIC_LISTENER_EVENT_STOP_COMPLETE stop_tx", listener);
            ctx.sig_stop.set(());
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
        let context = Box::new(QListenerCtx {
            _api: registration.api.clone(),
            ch: QQueue::new(),
            config: configuration.inner.clone(),
            state: Mutex::new(State::Idle),
            sig_stop: QResetChannel::new(),
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

    pub async fn start(&self, alpn: &[Buffer], local_address: &Addr) {
        let mut state = self.ctx.state.lock().unwrap();
        assert_eq!(*state, State::Idle);
        *state = State::Started;
        self.inner.inner.start(alpn, local_address)
    }

    pub async fn accept(&mut self) -> Option<QConnection> {
        let rx;
        {
            let state = self.ctx.state.lock().unwrap();
            if *state == State::Stopped {
                return None;
            }
            // must be started
            assert_ne!(*state, State::Idle);
            rx = self.ctx.ch.pop();
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
            let mut state = self.ctx.state.lock().unwrap();
            assert_eq!(*state, State::Started);
            *state = State::StopRequested;
            rx = self.ctx.sig_stop.reset();
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

// Experimental code

// extern "C" fn listener_handler2<T1: OnNewConnectionCallback, T2: OnStopCompleteCallback>(
//     listener: Handle,
//     context: *mut c_void,
//     event: &ListenerEvent,
// ) -> u32 {
//     assert!(!context.is_null());
//     let ctx = unsafe { (context as *mut QListnerCtx2<T1, T2>).as_mut().unwrap() };
//     let mut status = 0;
//     match event.event_type {
//         LISTENER_EVENT_NEW_CONNECTION => {
//             let raw = unsafe { event.payload.new_connection };
//             let h = raw.connection as Handle;
//             let info = unsafe { raw.info.as_ref().unwrap() };
//             info!(
//                 "[{:?}] LISTENER_EVENT_NEW_CONNECTION conn=[{:?}] info={:?}",
//                 listener, h, info
//             );
//             if let Some(cb) = ctx.on_new_connection.as_mut() {
//                 status = cb.invoke(h);
//             }
//         }
//         QUIC_LISTENER_EVENT_STOP_COMPLETE => {
//             info!("[{:?}] QUIC_LISTENER_EVENT_STOP_COMPLETE", listener);
//             if let Some(cb) = ctx.on_stop_complete.take() {
//                 status = cb.invoke();
//             }
//         }
//         _ => {
//             unreachable!()
//         }
//     }
//     status
// }

// // callback type for new connection
// pub trait OnNewConnectionCallback {
//     fn invoke(&mut self, h: Handle) -> u32;
// }

// pub trait OnStopCompleteCallback {
//     fn invoke(self) -> u32;
// }

// struct QListnerCtx2<T1: OnNewConnectionCallback, T2: OnStopCompleteCallback> {
//     on_new_connection: Option<T1>,
//     on_stop_complete: Option<T2>,
// }

// pub struct QListenerBase<T1: OnNewConnectionCallback, T2: OnStopCompleteCallback> {
//     _api: QApi,
//     inner: SBox<Listener>,
//     ctx: Box<QListnerCtx2<T1, T2>>,
// }

// impl<T1: OnNewConnectionCallback, T2: OnStopCompleteCallback> QListenerBase<T1, T2> {
//     // server open listener
//     pub fn open_inner(
//         registration: &QRegistration,
//         on_new_connection: Option<T1>,
//         on_stop_complete: Option<T2>,
//     ) -> Self {
//         let context = Box::new(QListnerCtx2 {
//             on_new_connection,
//             on_stop_complete,
//         });
//         let l = Listener::new(
//             &registration.inner.inner,
//             listener_handler2::<T1, T2>,
//             (&*context) as *const QListnerCtx2<T1, T2> as *const c_void,
//         );
//         Self {
//             _api: registration.api.clone(),
//             inner: SBox { inner: l },
//             ctx: context,
//         }
//     }
// }

// pub struct NewConnCallback {}

// impl OnNewConnectionCallback for NewConnCallback {
//     fn invoke(&mut self, _h: Handle) -> u32 {
//         todo!()
//     }
// }

// pub struct StopCompleteCallback {}

// impl OnStopCompleteCallback for StopCompleteCallback {
//     fn invoke(self) -> u32 {
//         todo!()
//     }
// }

// pub struct QListener2 {
//     inner: QListenerBase<NewConnCallback, StopCompleteCallback>,
// }

// impl QListener2 {
//     pub fn open(registration: &QRegistration, configuration: &QConfiguration) -> Self {
//         let on_new_connection = NewConnCallback {};
//         let on_stop_complete = StopCompleteCallback {};
//         let inner = QListenerBase::open_inner(
//             registration,
//             Some(on_new_connection),
//             Some(on_stop_complete),
//         );
//         Self { inner }
//     }
// }
