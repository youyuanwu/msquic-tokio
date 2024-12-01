use windows_core::HRESULT;

#[derive(Debug)]
pub struct QStatus(HRESULT);

impl QStatus {
    pub fn from_raw(ec: i32) -> Result<(), Self> {
        let hr = HRESULT::from_nt(ec);
        if hr.is_ok() {
            Ok(())
        } else {
            Err(QStatus(hr))
        }
    }

    pub fn error_from_raw(ec: i32) -> Result<(), std::io::Error> {
        Self::from_raw(ec).map_err(std::io::Error::from)
    }

    pub fn is_ok(&self) -> bool {
        self.0.is_ok()
    }
}

impl From<QStatus> for std::io::Error {
    fn from(value: QStatus) -> Self {
        Self::from_raw_os_error(value.0 .0)
    }
}
