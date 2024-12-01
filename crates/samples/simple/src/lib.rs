use std::{ffi::c_void, sync::Arc};

use c::{
    Microsoft::MsQuic::{
        MsQuicClose, MsQuicOpenVersion, HQUIC, QUIC_API_TABLE, QUIC_API_VERSION_2,
        QUIC_EXECUTION_PROFILE, QUIC_EXECUTION_PROFILE_LOW_LATENCY,
        QUIC_EXECUTION_PROFILE_TYPE_MAX_THROUGHPUT, QUIC_EXECUTION_PROFILE_TYPE_REAL_TIME,
        QUIC_EXECUTION_PROFILE_TYPE_SCAVENGER, QUIC_HANDLE, QUIC_REGISTRATION_CONFIG,
    },
    QStatus,
};
use windows_core::PCSTR;

struct ApiInner {
    inner: *const QUIC_API_TABLE,
}

unsafe impl Send for ApiInner {}
unsafe impl Sync for ApiInner {}

impl ApiInner {
    pub fn new() -> Self {
        let mut ret = ApiInner {
            inner: std::ptr::null(),
        };

        let ec = unsafe {
            MsQuicOpenVersion(
                QUIC_API_VERSION_2,
                std::ptr::addr_of_mut!(ret.inner) as *mut *mut c_void,
            )
        };
        c::QStatus::from_raw(ec).unwrap();
        assert_ne!(ret.inner, std::ptr::null());
        ret
    }

    pub fn get_api(&self) -> &QUIC_API_TABLE {
        unsafe { self.inner.as_ref() }.unwrap()
    }
}

impl Drop for ApiInner {
    fn drop(&mut self) {
        if self.inner.is_null() {
            return;
        }
        unsafe { MsQuicClose(self.inner as *const c_void) };
        self.inner = std::ptr::null();
    }
}

pub struct Handle {
    h: HQUIC,
}

impl Default for Handle {
    fn default() -> Self {
        Self::new()
    }
}

impl Handle {
    pub fn new() -> Self {
        Self {
            h: HQUIC::default(),
        }
    }

    pub fn put(&mut self) -> *mut *mut QUIC_HANDLE {
        std::ptr::addr_of_mut!(self.h) as *mut *mut QUIC_HANDLE
    }

    pub fn get(&self) -> *mut QUIC_HANDLE {
        self.h.0 as *mut QUIC_HANDLE
    }
}

// public api
pub struct Api {
    api: Arc<ApiInner>,
}

impl Default for Api {
    fn default() -> Self {
        Self::new()
    }
}

impl Api {
    pub fn new() -> Self {
        Self {
            api: Arc::new(ApiInner::new()),
        }
    }

    pub fn registration_open(&self, config: &RegistrationConfig) -> std::io::Result<Handle> {
        let arg = QUIC_REGISTRATION_CONFIG {
            AppName: PCSTR(config.app_name.as_ptr() as *const u8),
            ExecutionProfile: config.execution_profile.into(),
        };
        let f = self.api.get_api().RegistrationOpen.unwrap();
        let mut h = Handle::default();
        let hr = unsafe { f(&arg, h.put()) };
        QStatus::error_from_raw(hr)?;
        assert_ne!(h.h, HQUIC::default());
        Ok(h)
    }

    pub fn registation_close(&self, h: Handle) {
        let f = self.api.get_api().ConfigurationClose.unwrap();
        unsafe { f(h.get()) }
    }

    // pub fn configuration_open(&self, registration: Handle, settings: Settings, alpn: &[u8]) {
    //     todo!();
    //     let f = self.api.get_api().ConfigurationOpen.unwrap();
    //     let mut s = QUIC_SETTINGS {
    //         ..Default::default()
    //     };
    //     s.IdleTimeoutMs = 1000;
    //     // TODO: how to use c unions?
    //     //s.Anonymous1.IsSet.
    //     todo!()
    // }
}

#[derive(Clone, Copy)]
pub enum ExecutionProfile {
    LowLatency,
    MaxThroughput,
    RealTime,
    Scavenger,
}

impl From<ExecutionProfile> for QUIC_EXECUTION_PROFILE {
    fn from(value: ExecutionProfile) -> Self {
        match value {
            ExecutionProfile::LowLatency => QUIC_EXECUTION_PROFILE_LOW_LATENCY,
            ExecutionProfile::MaxThroughput => QUIC_EXECUTION_PROFILE_TYPE_MAX_THROUGHPUT,
            ExecutionProfile::RealTime => QUIC_EXECUTION_PROFILE_TYPE_REAL_TIME,
            ExecutionProfile::Scavenger => QUIC_EXECUTION_PROFILE_TYPE_SCAVENGER,
        }
    }
}

pub struct RegistrationConfig {
    // note that rust string is not null terminated so cstring is used.
    app_name: std::ffi::CString,
    execution_profile: ExecutionProfile,
}

pub struct Settings {}

#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use crate::{Api, RegistrationConfig};

    #[test]
    fn basic_test() {
        let api = Api::new();

        let config = RegistrationConfig {
            app_name: CString::new("testapp").unwrap(),
            execution_profile: crate::ExecutionProfile::LowLatency,
        };

        let rh = api.registration_open(&config).unwrap();
        api.registation_close(rh);
    }
}
