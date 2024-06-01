use std::slice;

use bytes::{Buf, BufMut, BytesMut};
use c2::Buffer;

use crate::SBox;

pub trait QOwnedBuffer {
    fn as_buff_ref(&self) -> QBuffRef;
    fn from_buff_ref(buff: &QBuffRef) -> Self;
}

pub struct QBuffRef<'a> {
    pub data: &'a [u8],
}

pub struct QArrayBuffer<const N: usize> {
    data: [u8; N],
}

// BuffRef can freely convert to or from Buffer.

impl<'a> From<&QBuffRef<'a>> for Buffer {
    fn from(value: &QBuffRef) -> Self {
        Buffer {
            length: value.data.len() as u32,
            buffer: value.data.as_ptr() as *mut u8,
        }
    }
}

// Note: the lifetime from raw is not tracked.
impl<'a> From<&Buffer> for QBuffRef<'a> {
    fn from(value: &Buffer) -> Self {
        let s =
            unsafe { std::slice::from_raw_parts(value.buffer, value.length.try_into().unwrap()) };
        Self { data: s }
    }
}

impl<'a, const N: usize> From<&'a QArrayBuffer<N>> for QBuffRef<'a> {
    fn from(value: &'a QArrayBuffer<N>) -> Self {
        value.as_buff_ref()
    }
}

impl<const N: usize> QOwnedBuffer for QArrayBuffer<N> {
    fn as_buff_ref(&self) -> QBuffRef<'_> {
        self.into()
    }

    fn from_buff_ref(value: &QBuffRef<'_>) -> Self {
        Self::from(value)
    }
}

impl<const N: usize> From<&QBuffRef<'_>> for QArrayBuffer<N> {
    fn from(value: &QBuffRef<'_>) -> Self {
        let s = value.data;
        let mut res = Self { data: [0; N] };
        res.data.copy_from_slice(s);
        res
    }
}

#[derive(PartialEq, Debug, Default)]
pub struct QVecBuffer {
    pub data: Vec<u8>,
}

impl From<&str> for QVecBuffer {
    fn from(value: &str) -> Self {
        Self {
            data: Vec::from(value),
        }
    }
}

impl<'a> From<&'a QVecBuffer> for QBuffRef<'a> {
    fn from(value: &'a QVecBuffer) -> Self {
        QBuffRef { data: &value.data }
    }
}

impl From<&QBuffRef<'_>> for QVecBuffer {
    fn from(value: &QBuffRef) -> Self {
        let s = value.data;
        // make a copy
        QVecBuffer { data: Vec::from(s) }
    }
}

impl QOwnedBuffer for QVecBuffer {
    fn as_buff_ref(&self) -> QBuffRef {
        self.into()
    }

    fn from_buff_ref(buff: &QBuffRef) -> Self {
        Self::from(buff)
    }
}

// used to call raw apis.
pub struct QBufferVec {
    data: Vec<Buffer>,
}

impl QBufferVec {
    pub fn as_buffers(&self) -> &[Buffer] {
        &self.data
    }
}

// convert of a vec of buffer to this to pass in raw api.
impl From<&[QVecBuffer]> for QBufferVec {
    fn from(value: &[QVecBuffer]) -> Self {
        Self {
            data: value
                .iter()
                .map(|x| {
                    let r = QBuffRef::from(x);
                    (&r).into()
                })
                .collect::<Vec<_>>(),
        }
    }
}

pub struct SBuffer {
    data: SBox<Buffer>,
}

impl From<&Buffer> for SBuffer {
    fn from(value: &Buffer) -> Self {
        SBuffer {
            data: SBox::new(*value),
        }
    }
}

impl From<&SBuffer> for Buffer {
    fn from(value: &SBuffer) -> Self {
        value.data.inner
    }
}

// Currently we copy the buf into vec for ease of code
pub struct QBufWrap {
    _inner: Vec<Vec<u8>>, // mem owner
    v: Vec<Buffer>,
}

unsafe impl Send for QBufWrap {}

impl QBufWrap {
    pub fn new(buf: impl Buf) -> Self {
        // make on heap so that no ptr move.
        let (vcs, vbs) = Self::convert_buf(buf);
        Self {
            _inner: vcs,
            v: vbs,
        }
    }

    fn convert_buf(mut b: impl Buf) -> (Vec<Vec<u8>>, Vec<Buffer>) {
        let mut vcs = Vec::new();
        let mut vbs = Vec::new();
        // change buf to vecs
        while b.has_remaining() {
            // copy
            let ck = b.chunk();
            let vc = Vec::from(ck);
            vbs.push(Buffer {
                length: vc.len() as u32,
                buffer: vc.as_ptr() as *mut u8,
            });
            vcs.push(vc);
            b.advance(ck.len());
        }
        (vcs, vbs)
    }

    pub fn as_buffs(&self) -> &[Buffer] {
        &self.v
    }
}

pub struct QBytesMut(pub BytesMut);

impl QBytesMut {
    pub fn from_buffs(b: &[Buffer]) -> Self {
        let mut res = BytesMut::new();
        b.iter().for_each(|i| {
            let s = unsafe { slice::from_raw_parts(i.buffer, i.length.try_into().unwrap()) };
            res.reserve(s.len());
            res.put_slice(s);
        });
        Self(res)
    }
}

pub fn debug_buf_to_string(mut b: impl Buf) -> String {
    let mut dst = vec![0; b.remaining()];
    b.copy_to_slice(&mut dst[..]);
    // let cp = b.copy_to_bytes(b.remaining());
    String::from_utf8_lossy(&dst).into_owned()
}

pub fn debug_raw_buf_to_string(b: Buffer) -> String {
    let s = String::from_utf8_lossy(unsafe {
        slice::from_raw_parts(b.buffer, b.length.try_into().unwrap())
    });
    s.into_owned()
}

#[cfg(test)]
mod test {

    use bytes::{BufMut, Bytes, BytesMut};
    use c2::Buffer;

    use crate::buffer::debug_raw_buf_to_string;

    use super::{debug_buf_to_string, QBufWrap, QBuffRef, QBufferVec, QBytesMut, QVecBuffer};

    #[test]
    fn test_vec_buffer() {
        let buff: QVecBuffer = "ok".into();
        let raw: Buffer = (&QBuffRef::from(&buff)).into();
        let buff2 = QVecBuffer::from(&QBuffRef::from(&raw));
        assert_eq!(buff.data, buff2.data);
    }

    #[test]
    fn test_buffer_vec() {
        let args: [QVecBuffer; 2] = [QVecBuffer::from("hi"), QVecBuffer::from("hi2")];
        let buffer_vec = QBufferVec::from(args.as_slice());

        let buffs = buffer_vec.as_buffers();
        assert_eq!(buffs.len(), 2);
        let b1 = &buffs[0];

        let arg1 = QVecBuffer::from(&QBuffRef::from(b1));
        assert_eq!(args[0], arg1);
    }

    #[test]
    fn test_buf() {
        let b = Bytes::from("mydata");
        let wrap = QBufWrap::new(Box::new(b));
        let v = wrap.as_buffs();
        assert_eq!(v.len(), 1);
        let b1 = v[0];
        let s = debug_raw_buf_to_string(b1);
        assert_eq!(s, "mydata");
    }

    #[test]
    fn test_buf2() {
        let mut b = BytesMut::with_capacity(5);
        b.put(&b"hello"[..]);
        b.put(&b"world"[..]); // this will grow
        let wrap = QBufWrap::new(Box::new(b));
        let v = wrap.as_buffs();
        assert_eq!(v.len(), 1);
        let b1 = v[0];
        let s = debug_raw_buf_to_string(b1);
        assert_eq!(s, "helloworld");
    }

    #[test]
    fn test_buf2str() {
        let args: [QVecBuffer; 2] = [QVecBuffer::from("hello"), QVecBuffer::from("world")];
        let buffer_vec = QBufferVec::from(args.as_slice());
        let bm = QBytesMut::from_buffs(buffer_vec.as_buffers());
        std::mem::drop(args);
        let s = debug_buf_to_string(bm.0);
        assert_eq!(s, "helloworld");
    }
}
