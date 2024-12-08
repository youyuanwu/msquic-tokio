//! Dynamically load shared lib
//!
//!

use crate::Microsoft::MsQuic::QUIC_API_TABLE;

#[cfg(target_os = "windows")]
const LIB_NAME: &str = "msquic.dll";

#[cfg(target_os = "linux")]
const LIB_NAME: &str = "libmsquic.so.2";

lazy_static::lazy_static! {
  static ref LIB: libloading::Library = unsafe { libloading::Library::new(LIB_NAME) }.expect("cannot load msquic shared lib");
  pub static ref MsQuicOpenVersion: libloading::Symbol<'static,unsafe extern "C" fn(u32, *mut *mut QUIC_API_TABLE) -> i32>
    = unsafe { LIB.get(b"MsQuicOpenVersion") }.expect("cannot load open fn");
  pub static ref MsQuicClose: libloading::Symbol<'static, unsafe extern "C" fn(*const QUIC_API_TABLE)>
    = unsafe { LIB.get(b"MsQuicClose") }.expect("cannot load close fn");
}
