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

    pub fn is_ok(&self) -> bool {
        self.0.is_ok()
    }
}
