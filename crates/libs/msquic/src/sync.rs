use std::{
    collections::LinkedList,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
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
        // if there is wait give out error
        if self.channel.can_set() {
            self.channel.set(Err(self.ec));
        }
    }
}

#[derive(Default)]
struct QWakableQueueState<T> {
    data: LinkedList<T>,
    waker: Option<Waker>,
    is_closed: bool,
}

#[derive(Clone)]
pub struct QWakableQueue<T> {
    state: Arc<Mutex<QWakableQueueState<T>>>,
}

impl<T> Default for QWakableQueue<T> {
    fn default() -> Self {
        let state = QWakableQueueState {
            data: LinkedList::new(),
            waker: None,
            is_closed: false,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }
}

impl<T> QWakableQueue<T> {
    // insert the data and wake
    pub fn insert(&mut self, data: T) {
        let mut lk = self.state.lock().unwrap();
        if lk.is_closed {
            panic!("set after close")
        }
        lk.data.push_back(data);
        if lk.waker.is_some() {
            lk.waker.take().unwrap().wake();
        }
    }

    // if polled none the res is cancelled and will not be delivered.
    // only one poll can happen at a time.
    pub fn poll(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<T>> {
        let mut lk = self.state.lock().unwrap();
        match lk.data.pop_front() {
            Some(d) => Poll::Ready(Some(d)),
            None => {
                if lk.is_closed {
                    Poll::Ready(None)
                } else {
                    // register waker
                    let prev = lk.waker.replace(cx.waker().clone());
                    assert!(prev.is_none());
                    Poll::Pending
                }
            }
        }
    }

    pub fn close(&mut self) {
        let mut lk = self.state.lock().unwrap();
        if lk.is_closed {
            return;
        }
        lk.is_closed = true;
        // ask for poll
        if let Some(w) = lk.waker.take() {
            w.wake();
        }
    }
}

struct QWakableSigState<T: Clone> {
    data: Option<T>,
    wakers: LinkedList<Waker>, // multiple waker can register
    is_closed: bool,
    frontend_pending: bool,
}

impl<T: Clone> Default for QWakableSigState<T> {
    fn default() -> Self {
        Self {
            data: Default::default(),
            wakers: Default::default(),
            is_closed: Default::default(),
            frontend_pending: Default::default(),
        }
    }
}

pub struct QWakableSig<T: Clone> {
    inner: Arc<Mutex<QWakableSigState<T>>>,
}

impl<T: Clone> Default for QWakableSig<T> {
    fn default() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl<T: Clone> QWakableSig<T> {
    // frontend has action pending. For example frontend initiated send
    pub fn set_frontend_pending(&mut self) {
        let mut lk = self.inner.lock().unwrap();
        assert!(!lk.frontend_pending);
        lk.frontend_pending = true;
    }

    pub fn is_frontend_pending(&self) -> bool {
        let lk = self.inner.lock().unwrap();
        lk.frontend_pending
    }

    // if none is returned, means the sig is cannceled.
    // Multiple waker can poll.
    pub fn poll(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<T>> {
        let mut lk = self.inner.lock().unwrap();
        match &lk.data {
            Some(s) => Poll::Ready(Some(s.clone())),
            None => {
                if lk.is_closed {
                    Poll::Ready(None)
                } else {
                    // save waker
                    lk.wakers.push_back(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }

    pub fn set(&mut self, data: T) {
        let mut lk = self.inner.lock().unwrap();
        if lk.data.is_some() {
            return; // already set
        }
        if lk.is_closed {
            panic!("set after close");
        }
        lk.data.replace(data);
        lk.frontend_pending = false; // the set corresponds to front end action, and we clear it here.
                                     // wake all wakers
        while let Some(w) = lk.wakers.pop_front() {
            w.wake();
        }
    }

    // reset to default state.
    pub fn reset(&mut self) {
        let mut lk = self.inner.lock().unwrap();
        if lk.is_closed {
            panic!("reset after close");
        }
        if !lk.wakers.is_empty() {
            panic!("reset while waker is pending");
        }
        lk.data = None;
    }

    pub fn close(&mut self) {
        let mut lk = self.inner.lock().unwrap();
        if lk.is_closed {
            return;
        }
        lk.is_closed = true;
        // wake all waker
        while let Some(w) = lk.wakers.pop_front() {
            w.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::poll_fn,
        sync::atomic::AtomicUsize,
        task::{Context, Poll},
        time::Duration,
    };

    use crate::sync::QWakableQueue;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    fn read_line(_cx: &mut Context<'_>) -> Poll<String> {
        println!("readline called");
        // the second poll should work
        if COUNTER.fetch_add(1, std::sync::atomic::Ordering::Acquire) < 1 {
            _cx.waker().clone().wake();
            Poll::Pending
        } else {
            Poll::Ready("Hello, World!".into())
        }
    }

    #[tokio::test]
    async fn poll_test() {
        let read_future = poll_fn(read_line);
        assert_eq!(read_future.await, "Hello, World!".to_owned());
    }

    #[tokio::test]
    async fn wake_test() {
        // set in same thread
        let mut wakable = QWakableQueue::default();
        wakable.insert(String::from("hello"));
        let fu = poll_fn(|cx| wakable.poll(cx));
        let out = fu.await.unwrap();
        assert_eq!(out, "hello");
    }

    #[tokio::test]
    async fn wake_test2() {
        // set from different task
        let mut wakable = QWakableQueue::default();
        let mut w_cp = wakable.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            w_cp.insert(String::from("hello"));
            w_cp.insert(String::from("hello2"));
        });
        let mut wakable_cp = wakable.clone();
        let fu = poll_fn(|cx| wakable_cp.poll(cx));
        let out = fu.await.unwrap();
        assert_eq!(out, "hello");
        let fu2 = poll_fn(|cx| wakable.poll(cx));
        let out2 = fu2.await.unwrap();
        assert_eq!(out2, "hello2");
    }

    #[tokio::test]
    async fn wake_test3() {
        // close
        let mut wakable: QWakableQueue<String> = Default::default();
        let mut w_cp = wakable.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            w_cp.close();
        });
        let fu = poll_fn(|cx| wakable.poll(cx));
        let out = fu.await;
        assert!(out.is_none());
    }
}
