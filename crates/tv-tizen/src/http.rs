//! HTTP(S) GET through the TV's `libcurl.so.4`, so binaries use the
//! firmware's OpenSSL and CA store instead of bundling a TLS stack.

use crate::{borrowed, c_string};
use std::io::Write;
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::sync::Once;

const CURLOPT_WRITEDATA: c_int = 10_001;
const CURLOPT_URL: c_int = 10_002;
const CURLOPT_USERAGENT: c_int = 10_018;
const CURLOPT_WRITEFUNCTION: c_int = 20_011;
const CURLOPT_TIMEOUT: c_int = 13;
const CURLOPT_FAILONERROR: c_int = 45;
const CURLOPT_FOLLOWLOCATION: c_int = 52;
const CURLOPT_NOSIGNAL: c_int = 99;
const CURLINFO_RESPONSE_CODE: c_int = 0x0020_0002;
const CURL_GLOBAL_DEFAULT: c_long = 3;

type WriteCallback = extern "C" fn(*const c_char, usize, usize, *mut c_void) -> usize;

#[link(name = "curl")]
extern "C" {
    fn curl_global_init(flags: c_long) -> c_int;
    fn curl_easy_init() -> *mut c_void;
    fn curl_easy_setopt(handle: *mut c_void, option: c_int, ...) -> c_int;
    fn curl_easy_perform(handle: *mut c_void) -> c_int;
    fn curl_easy_getinfo(handle: *mut c_void, info: c_int, ...) -> c_int;
    fn curl_easy_strerror(code: c_int) -> *const c_char;
    fn curl_easy_cleanup(handle: *mut c_void);
}

struct Sink<'a> {
    out: &'a mut dyn Write,
    written: u64,
    limit: u64,
    error: Option<String>,
}

extern "C" fn write_body(
    data: *const c_char,
    size: usize,
    count: usize,
    sink: *mut c_void,
) -> usize {
    // SAFETY: `sink` is the `Sink` passed as CURLOPT_WRITEDATA below and
    // `data` holds `size * count` bytes for the duration of this call.
    let sink = unsafe { &mut *sink.cast::<Sink>() };
    let length = size * count;
    if sink.written + length as u64 > sink.limit {
        sink.error = Some(format!("larger than {} bytes", sink.limit));
        return 0; // aborts the transfer with CURLE_WRITE_ERROR
    }
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
    if let Err(error) = sink.out.write_all(bytes) {
        sink.error = Some(error.to_string());
        return 0;
    }
    sink.written += length as u64;
    length
}

/// GET `url` (following redirects) and return the body. Fails on HTTP >= 400,
/// on a body larger than `max_bytes`, or after `timeout_secs`.
pub fn get(url: &str, max_bytes: usize, timeout_secs: u32) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    get_to(url, &mut body, max_bytes as u64, timeout_secs)?;
    Ok(body)
}

/// Stream a GET into `out`; returns the byte count. Same failure rules as
/// [`get`]; `out` may hold a partial body on error.
pub fn get_to(
    url: &str,
    out: &mut dyn Write,
    max_bytes: u64,
    timeout_secs: u32,
) -> Result<u64, String> {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        curl_global_init(CURL_GLOBAL_DEFAULT);
    });

    let url_c = c_string(url)?;
    let agent = c_string("tv-suite/0.1")?;
    let mut sink = Sink {
        out,
        written: 0,
        limit: max_bytes,
        error: None,
    };
    let handle = unsafe { curl_easy_init() };
    if handle.is_null() {
        return Err("curl_easy_init failed".into());
    }
    let callback: WriteCallback = write_body;
    let mut code;
    unsafe {
        curl_easy_setopt(handle, CURLOPT_URL, url_c.as_ptr());
        curl_easy_setopt(handle, CURLOPT_USERAGENT, agent.as_ptr());
        curl_easy_setopt(handle, CURLOPT_WRITEFUNCTION, callback);
        curl_easy_setopt(
            handle,
            CURLOPT_WRITEDATA,
            &mut sink as *mut Sink as *mut c_void,
        );
        curl_easy_setopt(handle, CURLOPT_FOLLOWLOCATION, 1 as c_long);
        curl_easy_setopt(handle, CURLOPT_FAILONERROR, 1 as c_long);
        curl_easy_setopt(handle, CURLOPT_NOSIGNAL, 1 as c_long);
        curl_easy_setopt(handle, CURLOPT_TIMEOUT, c_long::from(timeout_secs as i32));
        code = curl_easy_perform(handle);
    }
    let mut status: c_long = 0;
    unsafe {
        curl_easy_getinfo(handle, CURLINFO_RESPONSE_CODE, &mut status as *mut c_long);
        curl_easy_cleanup(handle);
    }
    if code == 0 && status >= 400 {
        code = -1;
    }
    if let Some(error) = sink.error {
        return Err(format!("GET {url}: {error}"));
    }
    match code {
        0 => Ok(sink.written),
        -1 => Err(format!("GET {url}: HTTP {status}")),
        _ => Err(format!("GET {url}: {} (HTTP {status})", unsafe {
            borrowed(curl_easy_strerror(code))
        })),
    }
}
