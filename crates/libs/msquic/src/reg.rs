use std::sync::Arc;

use c2::{Registration, RegistrationConfig};

use crate::{utils::SBox, QApi};

#[derive(Clone)]
pub struct QRegistration {
    pub api: QApi,
    pub inner: Arc<SBox<Registration>>,
}

impl QRegistration {
    pub fn new(api: &QApi, config: &RegistrationConfig) -> Self {
        let inner = Registration::new(&api.inner.inner, config);
        Self {
            api: api.clone(),
            inner: Arc::new(SBox::new(inner)),
        }
    }
}
