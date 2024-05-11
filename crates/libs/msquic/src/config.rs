use std::sync::Arc;

use c2::{Buffer, Configuration, CredentialConfig, Settings};

use crate::{reg::QRegistration, utils::SBox, QApi};

#[derive(Clone)]
pub struct QConfiguration {
    pub _api: QApi,
    pub inner: Arc<SBox<Configuration>>,
}

impl QConfiguration {
    pub fn new(reg: &QRegistration, alpn: &[Buffer], settings: &Settings) -> Self {
        let c = Configuration::new(&reg.inner.inner, alpn, settings);
        Self {
            _api: reg.api.clone(),
            inner: Arc::new(SBox::new(c)),
        }
    }
    pub fn load_cred(&self, cred_config: &CredentialConfig) {
        self.inner.inner.load_credential(cred_config);
    }
}
