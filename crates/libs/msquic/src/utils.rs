use std::fmt::Debug;

// sync and send wrapper
pub struct SBox<T> {
    pub inner: T,
}

impl<T> SBox<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn get_ref(&self) -> &T {
        &self.inner
    }
}

unsafe impl<T> Send for SBox<T> {}
unsafe impl<T> Sync for SBox<T> {}

impl<T> From<T> for SBox<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> Debug for SBox<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SBox")
            .field("inner", &"not displayed")
            .finish()
    }
}
