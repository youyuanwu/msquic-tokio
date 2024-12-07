use std::sync::Arc;

use msquic_sys2::Api;

use super::utils::SBox;

#[derive(Clone)]
pub struct QApi {
    pub inner: Arc<SBox<Api>>,
}

impl Default for QApi {
    fn default() -> Self {
        Self {
            inner: Arc::new(Api::new().into()),
        }
    }
}
