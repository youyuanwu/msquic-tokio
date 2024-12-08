/// Quic status
#[derive(Debug)]
pub struct QStatus(i32);

impl QStatus {
    pub fn from_raw(ec: i32) -> Result<(), Self> {
        let e = Self(ec);
        if e.is_success() {
            Ok(())
        } else {
            Err(e)
        }
    }

    pub fn error_from_raw(ec: i32) -> Result<(), std::io::Error> {
        Self::from_raw(ec).map_err(std::io::Error::from)
    }

    pub fn is_success(&self) -> bool {
        self.0 == 0
    }

    // pending and continue negative.
    // pub fn is_ok(&self) -> bool{
    //   self.0 <= 0
    // }
}

impl From<QStatus> for std::io::Error {
    fn from(value: QStatus) -> Self {
        Self::from_raw_os_error(value.0)
    }
}

#[cfg(test)]
mod tests {
    use super::QStatus;

    #[test]
    fn test_status() {
        let ok = QStatus::from_raw(0);
        assert!(ok.is_ok());

        let e = QStatus::error_from_raw(22).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    }
}
