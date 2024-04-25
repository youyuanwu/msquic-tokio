// sync and send wrapper
pub struct SBox<T> {
    pub inner: T,
}

impl<T> SBox<T> {
    pub fn new(inner: T) -> Self {
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
