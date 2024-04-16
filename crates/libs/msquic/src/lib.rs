#[cfg(test)]
mod tests {
    use std::{ffi::c_void, ptr::null_mut};

    use c::Microsoft::MsQuic::{MsQuicClose, MsQuicOpenVersion, QUIC_API_VERSION_2};

    #[test]
    fn api_table_test() {
        let mut table: *mut c_void = null_mut();
        unsafe { MsQuicOpenVersion(QUIC_API_VERSION_2, std::ptr::addr_of_mut!(table)) }.unwrap();

        unsafe { MsQuicClose(table) };
    }
}
