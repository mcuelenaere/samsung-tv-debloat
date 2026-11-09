//! RSA-SHA256 signature checks through the TV's OpenSSL 1.1 (`libcrypto`),
//! the same library its libcurl uses for TLS.

use std::os::raw::{c_int, c_uchar, c_void};
use std::ptr;

#[link(name = "crypto")]
extern "C" {
    fn BIO_new_mem_buf(buffer: *const c_void, length: c_int) -> *mut c_void;
    fn BIO_free(bio: *mut c_void) -> c_int;
    fn PEM_read_bio_PUBKEY(
        bio: *mut c_void,
        key: *mut *mut c_void,
        password: *const c_void,
        data: *mut c_void,
    ) -> *mut c_void;
    fn EVP_PKEY_free(key: *mut c_void);
    fn EVP_MD_CTX_new() -> *mut c_void;
    fn EVP_MD_CTX_free(context: *mut c_void);
    fn EVP_sha256() -> *const c_void;
    fn EVP_DigestVerifyInit(
        context: *mut c_void,
        key_context: *mut *mut c_void,
        digest: *const c_void,
        engine: *mut c_void,
        key: *mut c_void,
    ) -> c_int;
    fn EVP_DigestUpdate(context: *mut c_void, data: *const c_void, length: usize) -> c_int;
    fn EVP_DigestVerifyFinal(
        context: *mut c_void,
        signature: *const c_uchar,
        length: usize,
    ) -> c_int;
}

/// Whether `signature` is a valid RSASSA-PKCS1-v1_5 / SHA-256 signature of
/// `data` by the PEM `SubjectPublicKeyInfo` key. Errors only for an unusable
/// key.
pub fn verify_rsa_sha256(
    public_key_pem: &str,
    data: &[u8],
    signature: &[u8],
) -> Result<bool, String> {
    let length = c_int::try_from(public_key_pem.len()).map_err(|_| "key too long")?;
    unsafe {
        let bio = BIO_new_mem_buf(public_key_pem.as_ptr().cast(), length);
        if bio.is_null() {
            return Err("BIO_new_mem_buf failed".into());
        }
        let key = PEM_read_bio_PUBKEY(bio, ptr::null_mut(), ptr::null(), ptr::null_mut());
        BIO_free(bio);
        if key.is_null() {
            return Err("unreadable public key".into());
        }
        let context = EVP_MD_CTX_new();
        let valid = !context.is_null()
            && EVP_DigestVerifyInit(context, ptr::null_mut(), EVP_sha256(), ptr::null_mut(), key)
                == 1
            && EVP_DigestUpdate(context, data.as_ptr().cast(), data.len()) == 1
            && EVP_DigestVerifyFinal(context, signature.as_ptr(), signature.len()) == 1;
        EVP_MD_CTX_free(context);
        EVP_PKEY_free(key);
        Ok(valid)
    }
}
