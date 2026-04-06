use crate::{
    Result,
    input::{PcmEnvelope, WavSpec, append_encoded_sample, container_bits_from_valid_bits},
};

#[cfg_attr(not(test), allow(dead_code))]
const CHUNK_CAPACITY: usize = 64 * 1024;
const ZERO_MD5: [u8; 16] = [0; 16];
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const EMPTY_STREAM_MD5: [u8; 16] = [
    0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42, 0x7e,
];

pub(crate) struct Md5 {
    inner: Md5Inner,
}

enum Md5Inner {
    #[cfg(windows)]
    Windows(WindowsMd5),
    Pure(PureMd5),
}

impl Md5 {
    pub(crate) fn new() -> Self {
        #[cfg(windows)]
        if let Some(md5) = WindowsMd5::new() {
            return Self {
                inner: Md5Inner::Windows(md5),
            };
        }

        Self {
            inner: Md5Inner::Pure(PureMd5::new()),
        }
    }

    pub(crate) fn update(&mut self, input: &[u8]) {
        match &mut self.inner {
            #[cfg(windows)]
            Md5Inner::Windows(md5) => md5.update(input),
            Md5Inner::Pure(md5) => md5.update(input),
        }
    }

    pub(crate) fn finalize(self) -> [u8; 16] {
        match self.inner {
            #[cfg(windows)]
            Md5Inner::Windows(md5) => md5.finalize(),
            Md5Inner::Pure(md5) => md5.finalize(),
        }
    }
}

#[cfg(windows)]
use std::{ffi::c_void, mem::size_of, ptr, sync::OnceLock};

#[cfg(windows)]
struct Md5Algorithm {
    handle: usize,
    object_length: u32,
}

#[cfg(windows)]
struct WindowsMd5 {
    handle: *mut c_void,
    _object: Vec<u8>,
}

#[cfg(windows)]
impl WindowsMd5 {
    fn new() -> Option<Self> {
        let algorithm = md5_algorithm()?;
        let mut hash = ptr::null_mut();
        let mut object = vec![0u8; algorithm.object_length as usize];
        let status = unsafe {
            BCryptCreateHash(
                algorithm.handle as *mut c_void,
                &mut hash,
                object.as_mut_ptr(),
                object.len() as u32,
                ptr::null_mut(),
                0,
                0,
            )
        };
        if nt_success(status) {
            Some(Self {
                handle: hash,
                _object: object,
            })
        } else {
            None
        }
    }

    fn update(&mut self, mut input: &[u8]) {
        while !input.is_empty() {
            let chunk_len = input.len().min(u32::MAX as usize);
            let status = unsafe {
                BCryptHashData(self.handle, input.as_ptr().cast_mut(), chunk_len as u32, 0)
            };
            assert!(nt_success(status), "BCryptHashData failed: {status:#x}");
            input = &input[chunk_len..];
        }
    }

    fn finalize(mut self) -> [u8; 16] {
        let mut digest = [0u8; 16];
        let status =
            unsafe { BCryptFinishHash(self.handle, digest.as_mut_ptr(), digest.len() as u32, 0) };
        assert!(nt_success(status), "BCryptFinishHash failed: {status:#x}");
        let status = unsafe { BCryptDestroyHash(self.handle) };
        debug_assert!(nt_success(status), "BCryptDestroyHash failed: {status:#x}");
        self.handle = ptr::null_mut();
        digest
    }
}

#[cfg(windows)]
impl Drop for WindowsMd5 {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            let _ = unsafe { BCryptDestroyHash(self.handle) };
        }
    }
}

#[cfg(windows)]
fn md5_algorithm() -> Option<&'static Md5Algorithm> {
    static ALGORITHM: OnceLock<Option<Md5Algorithm>> = OnceLock::new();
    ALGORITHM
        .get_or_init(|| {
            let mut handle = ptr::null_mut();
            let status = unsafe {
                BCryptOpenAlgorithmProvider(
                    &mut handle,
                    BCRYPT_MD5_ALGORITHM.as_ptr(),
                    ptr::null(),
                    0,
                )
            };
            if !nt_success(status) {
                return None;
            }

            let object_length = get_u32_property(handle, BCRYPT_OBJECT_LENGTH)?;
            let hash_length = get_u32_property(handle, BCRYPT_HASH_LENGTH)?;
            if hash_length != 16 {
                return None;
            }

            Some(Md5Algorithm {
                handle: handle as usize,
                object_length,
            })
        })
        .as_ref()
}

#[cfg(windows)]
fn get_u32_property(handle: *mut c_void, property: &[u16]) -> Option<u32> {
    let mut value = 0u32;
    let mut written = 0u32;
    let status = unsafe {
        BCryptGetProperty(
            handle,
            property.as_ptr(),
            (&mut value as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut written,
            0,
        )
    };
    (nt_success(status) && written == size_of::<u32>() as u32).then_some(value)
}

#[cfg(windows)]
const BCRYPT_MD5_ALGORITHM: &[u16] = &[b'M' as u16, b'D' as u16, b'5' as u16, 0];
#[cfg(windows)]
const BCRYPT_OBJECT_LENGTH: &[u16] = &[
    b'O' as u16,
    b'b' as u16,
    b'j' as u16,
    b'e' as u16,
    b'c' as u16,
    b't' as u16,
    b'L' as u16,
    b'e' as u16,
    b'n' as u16,
    b'g' as u16,
    b't' as u16,
    b'h' as u16,
    0,
];
#[cfg(windows)]
const BCRYPT_HASH_LENGTH: &[u16] = &[
    b'H' as u16,
    b'a' as u16,
    b's' as u16,
    b'h' as u16,
    b'D' as u16,
    b'i' as u16,
    b'g' as u16,
    b'e' as u16,
    b's' as u16,
    b't' as u16,
    b'L' as u16,
    b'e' as u16,
    b'n' as u16,
    b'g' as u16,
    b't' as u16,
    b'h' as u16,
    0,
];

#[cfg(windows)]
fn nt_success(status: i32) -> bool {
    status >= 0
}

#[cfg(windows)]
#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptOpenAlgorithmProvider(
        ph_algorithm: *mut *mut c_void,
        psz_alg_id: *const u16,
        psz_implementation: *const u16,
        dw_flags: u32,
    ) -> i32;
    fn BCryptGetProperty(
        h_object: *mut c_void,
        psz_property: *const u16,
        pb_output: *mut u8,
        cb_output: u32,
        pcb_result: *mut u32,
        dw_flags: u32,
    ) -> i32;
    fn BCryptCreateHash(
        h_algorithm: *mut c_void,
        ph_hash: *mut *mut c_void,
        pb_hash_object: *mut u8,
        cb_hash_object: u32,
        pb_secret: *mut u8,
        cb_secret: u32,
        dw_flags: u32,
    ) -> i32;
    fn BCryptHashData(h_hash: *mut c_void, pb_input: *mut u8, cb_input: u32, dw_flags: u32) -> i32;
    fn BCryptFinishHash(
        h_hash: *mut c_void,
        pb_output: *mut u8,
        cb_output: u32,
        dw_flags: u32,
    ) -> i32;
    fn BCryptDestroyHash(h_hash: *mut c_void) -> i32;
}

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const K: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

struct PureMd5 {
    state: [u32; 4],
    buffer: [u8; 64],
    buffer_len: usize,
    processed_bytes: u64,
}

impl PureMd5 {
    fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buffer: [0; 64],
            buffer_len: 0,
            processed_bytes: 0,
        }
    }

    fn update(&mut self, mut input: &[u8]) {
        self.processed_bytes += input.len() as u64;

        if self.buffer_len > 0 {
            let copy_len = (64 - self.buffer_len).min(input.len());
            self.buffer[self.buffer_len..self.buffer_len + copy_len]
                .copy_from_slice(&input[..copy_len]);
            self.buffer_len += copy_len;
            input = &input[copy_len..];
            if self.buffer_len == 64 {
                let block = self.buffer;
                self.process_block(&block);
                self.buffer_len = 0;
            }
        }

        while input.len() >= 64 {
            let block: [u8; 64] = input[..64].try_into().expect("exact md5 chunk length");
            self.process_block(&block);
            input = &input[64..];
        }

        if !input.is_empty() {
            self.buffer[..input.len()].copy_from_slice(input);
            self.buffer_len = input.len();
        }
    }

    fn finalize(mut self) -> [u8; 16] {
        let bit_len = self.processed_bytes * 8;
        let mut padding = [0u8; 72];
        padding[0] = 0x80;
        let padding_len = if self.buffer_len < 56 {
            56 - self.buffer_len
        } else {
            64 + 56 - self.buffer_len
        };
        self.update(&padding[..padding_len]);
        self.update(&bit_len.to_le_bytes());

        let mut digest = [0u8; 16];
        for (index, word) in self.state.iter().enumerate() {
            digest[index * 4..(index + 1) * 4].copy_from_slice(&word.to_le_bytes());
        }
        digest
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        let mut words = [0u32; 16];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_le_bytes(chunk.try_into().expect("exact md5 word"));
        }

        let [mut a, mut b, mut c, mut d] = self.state;

        for index in 0..64 {
            let (f, g) = match index {
                0..=15 => ((b & c) | ((!b) & d), index),
                16..=31 => ((d & b) | ((!d) & c), (5 * index + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };

            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[index])
                    .wrapping_add(words[g])
                    .rotate_left(S[index]),
            );
            a = temp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn digest_bytes(bytes: &[u8]) -> [u8; 16] {
    let mut md5 = Md5::new();
    md5.update(bytes);
    md5.finalize()
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn streaminfo_md5(spec: WavSpec, samples: &[i32]) -> Result<[u8; 16]> {
    let envelope = PcmEnvelope {
        channels: u16::from(spec.channels),
        valid_bits_per_sample: u16::from(spec.bits_per_sample),
        container_bits_per_sample: container_bits_from_valid_bits(u16::from(spec.bits_per_sample)),
        channel_mask: spec.channel_mask,
    };
    let mut md5 = Md5::new();
    let mut buffer = Vec::with_capacity(CHUNK_CAPACITY);

    for &sample in samples {
        append_encoded_sample(&mut buffer, sample, envelope)?;
        if buffer.len() >= CHUNK_CAPACITY {
            md5.update(&buffer);
            buffer.clear();
        }
    }

    if !buffer.is_empty() {
        md5.update(&buffer);
    }

    Ok(md5.finalize())
}

#[allow(dead_code)]
pub(crate) fn verify_streaminfo_md5(
    spec: WavSpec,
    samples: &[i32],
    expected_md5: [u8; 16],
) -> Result<()> {
    verify_streaminfo_digest(streaminfo_md5(spec, samples)?, expected_md5)
}

pub(crate) fn verify_streaminfo_digest(actual_md5: [u8; 16], expected_md5: [u8; 16]) -> Result<()> {
    if expected_md5 == ZERO_MD5 {
        return Ok(());
    }

    if actual_md5 == expected_md5 {
        Ok(())
    } else {
        Err(crate::Error::InvalidFlac("STREAMINFO MD5 mismatch"))
    }
}

#[cfg(test)]
mod tests {
    use super::{EMPTY_STREAM_MD5, digest_bytes, streaminfo_md5};
    use crate::input::{WavSpec, ordinary_channel_mask};

    #[test]
    fn digest_matches_empty_vector() {
        assert_eq!(digest_bytes(b""), EMPTY_STREAM_MD5);
    }

    #[test]
    fn digest_matches_abc_vector() {
        assert_eq!(
            digest_bytes(b"abc"),
            [
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, 0x72,
            ]
        );
    }

    #[test]
    fn streaminfo_md5_matches_simple_16bit_pcm_fixture() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 4,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(1).unwrap(),
        };
        let samples = [1, -2, 3, -4];

        assert_eq!(
            streaminfo_md5(spec, &samples).unwrap(),
            [
                0x4e, 0xee, 0x3c, 0x56, 0x22, 0x45, 0x41, 0xfe, 0x00, 0x81, 0x1d, 0x91, 0xd5, 0x24,
                0x24, 0x56,
            ]
        );
    }
}
