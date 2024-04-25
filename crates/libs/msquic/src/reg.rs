use c2::{Registration, RegistrationConfig};

use crate::{utils::SBox, QApi};

pub struct QRegistration {
    pub api: QApi,
    pub inner: SBox<Registration>,
}

impl QRegistration {
    pub fn new(api: &QApi, config: &RegistrationConfig) -> Self {
        let inner = Registration::new(&api.inner.inner, config);
        Self {
            api: api.clone(),
            inner: inner.into(),
        }
    }
}
