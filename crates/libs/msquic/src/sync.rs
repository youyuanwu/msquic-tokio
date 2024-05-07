use std::{
    collections::LinkedList,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::sync::oneshot::{self, Receiver};

pub type QError = u32;

pub struct QReceiver<T> {
    rx: oneshot::Receiver<T>,
}

pub struct QSender<T> {
    tx: oneshot::Sender<T>,
}

pub fn oneshot_channel<T>() -> (QSender<T>, QReceiver<T>) {
    let (tx, rx) = oneshot::channel();
    (QSender { tx }, QReceiver { rx })
}

impl<T> QSender<T> {
    pub fn send(self, data: T) {
        // ignore is receiver dropped.
        let _ = self.tx.send(data);
    }
}

impl<T> QReceiver<T> {
    pub fn blocking_recv(self) -> T {
        // sender must send stuff so that there is not error.
        self.rx.blocking_recv().unwrap()
    }

    // make a receiver that is filled with data
    fn make_ready(data: T) -> Self {
        let (tx, rx) = oneshot_channel();
        tx.send(data);
        rx
    }
}

impl<T> Future for QReceiver<T> {
    type Output = T;
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Try to receive the value from the sender
        let innner = <Receiver<T> as Future>::poll(Pin::new(&mut self.rx), _cx);
        match innner {
            Poll::Ready(x) => {
                // error only happens when sender is dropped without sending.
                // we ignore this error since in sf-rs use this will never happen.
                Poll::Ready(x.expect("sender closed without sending a value."))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// signal that can be set and reset and awaited.
pub struct QResetChannel<T> {
    tx: Option<QSender<T>>,
}

impl<T> Default for QResetChannel<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> QResetChannel<T> {
    pub fn new() -> Self {
        Self { tx: None }
    }

    // set and reset are not thread safe.
    pub fn reset(&mut self) -> QReceiver<T> {
        let (tx, rx) = oneshot_channel();
        assert!(self.tx.is_none());
        self.tx.replace(tx);
        rx
    }

    // send the data
    pub fn set(&mut self, data: T) {
        assert!(self.tx.is_some());
        self.tx.take().unwrap().send(data)
    }

    // check if the channel can send/set
    pub fn can_set(&self) -> bool {
        self.tx.is_some()
    }
}

pub type QSignal = QResetChannel<()>;

// Queue that can insert and awaited.
pub struct QQueue<T> {
    v: LinkedList<T>,
    channel: QResetChannel<Result<T, QError>>,
    is_closed: bool,
    ec: u32,
}

impl<T> Default for QQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> QQueue<T> {
    pub fn new() -> Self {
        Self {
            v: LinkedList::new(),
            channel: QResetChannel::new(),
            is_closed: false,
            ec: 0,
        }
    }

    pub fn insert(&mut self, data: T) {
        assert!(!self.is_closed);
        // if channel is waiting insert to channel.
        if self.channel.can_set() {
            self.channel.set(Ok(data));
            return;
        }
        self.v.push_back(data);
    }

    // fails if queue is closed and there is no more data
    // i.e. no more new data can be extracted.
    pub fn pop(&mut self) -> QReceiver<Result<T, QError>> {
        // if there is pending in v, return it
        if !self.v.is_empty() {
            let data = self.v.pop_front().unwrap();
            return QReceiver::make_ready(Ok(data));
        }

        if self.is_closed {
            // copy the error code
            return QReceiver::make_ready(Err(self.ec));
        }

        // wait for next insert.
        assert!(!self.channel.can_set()); // no pending wait
        self.channel.reset()
    }

    // no more pop can initiate new wait.
    //
    pub fn close(&mut self, err: QError) {
        self.is_closed = true;
        self.ec = err;
        if self.v.is_empty() {
            // if there is wait give out error
            if self.channel.can_set() {
                self.channel.set(Err(self.ec));
            }
        } else {
            assert!(self.channel.can_set(), "v is empty and channel is waiting");
        }
    }
}
