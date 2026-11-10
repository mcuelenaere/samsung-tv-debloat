//! The HTTP/1.1 half of the YouTube proxy: message heads, body framing, and
//! the one page (`/tv`) that gets the TizenTube script tag.

use std::io::{self, BufRead, BufReader, Read, Write};

/// Where the proxy serves TizenTube, on the www.youtube.com origin.
pub const SCRIPT_PATH: &str = "/tv-suite/tizentube.js";
pub const MAX_PAGE: usize = 8 << 20;
const MAX_HEAD: usize = 64 * 1024;
/// Chunk-size and trailer lines.
const MAX_LINE: u64 = 4096;

/// Cobalt closes a page without a CSP header, so YouTube's is replaced by
/// one that allows the injected script (as Vetro does).
pub const CSP: &str = "default-src * data: blob: 'unsafe-inline' 'unsafe-eval'; \
    img-src * data: blob:; media-src * data: blob:; \
    script-src * data: blob: 'unsafe-inline' 'unsafe-eval'; \
    style-src * data: blob: 'unsafe-inline'; connect-src *; font-src * data:";

pub struct Script {
    pub version: String,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    /// Request or status line.
    pub line: String,
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn remove(&mut self, name: &str) {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
    }

    pub fn set(&mut self, name: &str, value: &str) {
        self.remove(name);
        self.headers.push((name.to_owned(), value.to_owned()));
    }

    /// Whether a comma-separated header lists `token`.
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .flat_map(|(_, value)| value.split(','))
            .any(|item| item.trim().eq_ignore_ascii_case(token))
    }

    pub fn status(&self) -> u16 {
        self.line
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0)
    }

    pub fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        let mut text = format!("{}\r\n", self.line);
        for (key, value) in &self.headers {
            text.push_str(&format!("{key}: {value}\r\n"));
        }
        text.push_str("\r\n");
        writer.write_all(text.as_bytes())
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn read_line(reader: &mut impl BufRead, limit: u64) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    reader.take(limit).read_until(b'\n', &mut line)?;
    if !line.is_empty() && !line.ends_with(b"\n") {
        return Err(if line.len() as u64 >= limit {
            invalid("line too long")
        } else {
            io::ErrorKind::UnexpectedEof.into()
        });
    }
    Ok(line)
}

/// The next message head, or `None` at a clean EOF before it.
pub fn read_head(reader: &mut impl BufRead) -> io::Result<Option<Head>> {
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0;
    loop {
        // Header lines can be long (YouTube's CSP); the head as a whole is
        // bounded.
        let line = read_line(reader, (MAX_HEAD - total) as u64 + 1)?;
        if line.is_empty() {
            return if lines.is_empty() {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        total += line.len();
        if total > MAX_HEAD {
            return Err(invalid("head too large"));
        }
        let text = String::from_utf8_lossy(&line)
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        if text.is_empty() {
            if lines.is_empty() {
                continue; // stray CRLF between messages
            }
            break;
        }
        lines.push(text);
    }
    let mut lines = lines.into_iter();
    let line = lines.next().unwrap_or_default();
    let headers = lines
        .map(|header| {
            header
                .split_once(':')
                .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
                .ok_or_else(|| invalid("bad header line"))
        })
        .collect::<io::Result<_>>()?;
    Ok(Some(Head { line, headers }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    Empty,
    Length(u64),
    Chunked,
    UntilClose,
}

fn length(head: &Head) -> io::Result<Option<u64>> {
    head.get("content-length")
        .map(|value| value.parse().map_err(|_| invalid("bad content-length")))
        .transpose()
}

pub fn request_body(head: &Head) -> io::Result<Body> {
    if head.has_token("transfer-encoding", "chunked") {
        return Ok(Body::Chunked);
    }
    Ok(length(head)?.map_or(Body::Empty, Body::Length))
}

pub fn response_body(method: &str, head: &Head) -> io::Result<Body> {
    let status = head.status();
    if method == "HEAD" || status / 100 == 1 || status == 204 || status == 304 {
        return Ok(Body::Empty);
    }
    if head.has_token("transfer-encoding", "chunked") {
        return Ok(Body::Chunked);
    }
    Ok(length(head)?.map_or(Body::UntilClose, Body::Length))
}

fn chunk_size(line: &[u8]) -> io::Result<u64> {
    let text = String::from_utf8_lossy(line);
    let size = text.trim().split(';').next().unwrap_or("").trim();
    u64::from_str_radix(size, 16).map_err(|_| invalid("bad chunk size"))
}

fn copy_exact(reader: &mut impl BufRead, writer: &mut impl Write, length: u64) -> io::Result<()> {
    if io::copy(&mut reader.take(length), writer)? != length {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

/// Forward a body unchanged, framing included.
pub fn copy_body(reader: &mut impl BufRead, writer: &mut impl Write, body: Body) -> io::Result<()> {
    match body {
        Body::Empty => Ok(()),
        Body::Length(length) => copy_exact(reader, writer, length),
        Body::UntilClose => io::copy(reader, writer).map(drop),
        Body::Chunked => loop {
            let line = read_line(reader, MAX_LINE)?;
            writer.write_all(&line)?;
            let size = chunk_size(&line)?;
            if size == 0 {
                loop {
                    let trailer = read_line(reader, MAX_LINE)?;
                    writer.write_all(&trailer)?;
                    if trailer.is_empty() || trailer == b"\r\n" || trailer == b"\n" {
                        return Ok(());
                    }
                }
            }
            copy_exact(reader, writer, size + 2)?; // data and its CRLF
        },
    }
}

/// A whole body, unframed, at most `maximum` bytes.
pub fn read_body(reader: &mut impl BufRead, body: Body, maximum: usize) -> io::Result<Vec<u8>> {
    let too_large = || invalid("body too large");
    let mut data = Vec::new();
    match body {
        Body::Empty => {}
        Body::Length(length) => {
            if length > maximum as u64 {
                return Err(too_large());
            }
            copy_exact(reader, &mut data, length)?;
        }
        Body::UntilClose => {
            reader.take(maximum as u64 + 1).read_to_end(&mut data)?;
            if data.len() > maximum {
                return Err(too_large());
            }
        }
        Body::Chunked => loop {
            let size = chunk_size(&read_line(reader, MAX_LINE)?)?;
            if size == 0 {
                while !matches!(
                    read_line(reader, MAX_LINE)?.as_slice(),
                    b"" | b"\r\n" | b"\n"
                ) {}
                break;
            }
            if data.len() as u64 + size > maximum as u64 {
                return Err(too_large());
            }
            copy_exact(reader, &mut data, size)?;
            read_line(reader, MAX_LINE)?;
        },
    }
    Ok(data)
}

/// YouTube's TV start page: `/tv`, `/tv?...` or `/tv#...`, not `/tv_config`.
pub fn is_tv_page(target: &str) -> bool {
    target == "/tv" || target.starts_with("/tv?") || target.starts_with("/tv#")
}

pub fn script_tag(version: &str) -> String {
    let version: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .collect();
    format!("<script src=\"{SCRIPT_PATH}?v={version}\"></script>")
}

/// The page without CSP `<meta>` tags and with `tag` before `</body>` (or at
/// the end without one).
pub fn rewrite_page(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() + tag.len());
    let mut at = 0;
    while let Some(start) = lower[at..].find("<meta").map(|offset| at + offset) {
        let Some(end) = lower[start..].find('>').map(|offset| start + offset + 1) else {
            break;
        };
        out.push_str(&html[at..start]);
        if !lower[start..end].contains("content-security-policy") {
            out.push_str(&html[start..end]);
        }
        at = end;
    }
    out.push_str(&html[at..]);
    match out.to_ascii_lowercase().find("</body>") {
        Some(position) => out.insert_str(position, tag),
        None => out.push_str(tag),
    }
    out
}

fn respond(writer: &mut impl Write, status: &str, kind: &str, body: &[u8]) -> io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\ncache-control: no-store\r\n\r\n",
        body.len()
    )?;
    writer.write_all(body)
}

/// Relay one decrypted connection from Cobalt to www.youtube.com, adding
/// the script to the TV page and serving it at [`SCRIPT_PATH`]. The upstream
/// connection is opened on the first request that needs it.
pub fn relay<C: Read + Write, U: Read + Write>(
    client: C,
    mut connect: impl FnMut() -> io::Result<U>,
    script: Option<&Script>,
    mut on_page: impl FnMut(),
) -> io::Result<()> {
    let mut client = BufReader::new(client);
    let mut upstream: Option<BufReader<U>> = None;
    loop {
        let Some(mut request) = read_head(&mut client)? else {
            return Ok(());
        };
        let mut words = request.line.split(' ');
        let method = words.next().unwrap_or_default().to_owned();
        let target = words
            .next()
            .ok_or_else(|| invalid("bad request line"))?
            .to_owned();
        let request_framing = request_body(&request)?;
        let path = target.split(['?', '#']).next().unwrap_or_default();
        if path == SCRIPT_PATH {
            copy_body(&mut client, &mut io::sink(), request_framing)?;
            match script {
                Some(script) if method == "GET" => respond(
                    client.get_mut(),
                    "200 OK",
                    "application/javascript; charset=utf-8",
                    &script.body,
                )?,
                _ => respond(client.get_mut(), "404 Not Found", "text/plain", b"")?,
            }
            client.get_mut().flush()?;
            continue;
        }
        let page = method == "GET" && is_tv_page(&target) && script.is_some();
        if page {
            request.set("Accept-Encoding", "identity");
        }
        let up = match upstream.as_mut() {
            Some(up) => up,
            None => upstream.insert(BufReader::new(connect()?)),
        };
        request.write_to(up.get_mut())?;
        copy_body(&mut client, up.get_mut(), request_framing)?;
        up.get_mut().flush()?;

        let mut response = loop {
            let head = read_head(up)?.ok_or(io::ErrorKind::UnexpectedEof)?;
            if head.status() / 100 == 1 && head.status() != 101 {
                head.write_to(client.get_mut())?; // 100 Continue and friends
                continue;
            }
            break head;
        };
        if response.status() == 101 {
            return Err(io::Error::other("protocol upgrades are not proxied"));
        }
        let framing = response_body(&method, &response)?;
        let close = framing == Body::UntilClose
            || response.has_token("connection", "close")
            || request.has_token("connection", "close");
        response.remove("alt-svc"); // QUIC would bypass the proxy
        let html = response
            .get("content-type")
            .is_some_and(|kind| kind.contains("text/html"));
        match script {
            Some(script) if page && response.status() == 200 && html => {
                let body = read_body(up, framing, MAX_PAGE)?;
                let rewritten = rewrite_page(
                    &String::from_utf8_lossy(&body),
                    &script_tag(&script.version),
                );
                for name in [
                    "content-security-policy",
                    "content-security-policy-report-only",
                    "content-length",
                    "content-encoding",
                    "transfer-encoding",
                ] {
                    response.remove(name);
                }
                response.set("Content-Security-Policy", CSP);
                response.set("Content-Length", &rewritten.len().to_string());
                response.write_to(client.get_mut())?;
                client.get_mut().write_all(rewritten.as_bytes())?;
                on_page();
            }
            _ => {
                response.write_to(client.get_mut())?;
                copy_body(up, client.get_mut(), framing)?;
            }
        }
        client.get_mut().flush()?;
        if close {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::rc::Rc;

    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Rc<RefCell<Vec<u8>>>,
    }

    impl Read for Duplex {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.borrow_mut().extend_from_slice(buffer);
            Ok(buffer.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn duplex(input: &str) -> (Duplex, Rc<RefCell<Vec<u8>>>) {
        let output = Rc::new(RefCell::new(Vec::new()));
        let stream = Duplex {
            input: Cursor::new(input.as_bytes().to_vec()),
            output: output.clone(),
        };
        (stream, output)
    }

    #[test]
    fn injects_into_the_tv_page_and_serves_the_script() {
        let (client, to_client) = duplex(
            "GET /tv?launch=menu HTTP/1.1\r\nHost: www.youtube.com\r\nAccept-Encoding: gzip, br\r\n\r\n\
             GET /tv-suite/tizentube.js?v=1.0 HTTP/1.1\r\nHost: www.youtube.com\r\n\r\n\
             POST /youtubei/v1/browse HTTP/1.1\r\nHost: www.youtube.com\r\nContent-Length: 2\r\n\r\n{}",
        );
        let page = "<html><head><META http-equiv=\"Content-Security-Policy\" content=\"x\"></head>\
                    <body>hi</body></html>";
        let (upstream, to_upstream) = duplex(&format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Security-Policy: script-src 'self'\r\nAlt-Svc: h3=\":443\"\r\n\
             Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{page}\r\n0\r\n\r\n\
             HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\n\r\n[42]",
            page.len()
        ));
        let mut upstream = Some(upstream);
        let script = Script {
            version: "1.0".into(),
            body: b"tizentube()".to_vec(),
        };
        let mut pages = 0;
        relay(
            client,
            || upstream.take().ok_or(io::ErrorKind::NotConnected.into()),
            Some(&script),
            || pages += 1,
        )
        .unwrap();

        let sent = String::from_utf8(to_upstream.borrow().clone()).unwrap();
        assert!(sent.contains("Accept-Encoding: identity\r\n"), "{sent}");
        assert!(sent.ends_with("Content-Length: 2\r\n\r\n{}"), "{sent}");
        let received = String::from_utf8(to_client.borrow().clone()).unwrap();
        let expected_page = "<html><head></head><body>hi\
                             <script src=\"/tv-suite/tizentube.js?v=1.0\"></script></body></html>";
        assert!(received.contains(&format!("Content-Length: {}\r\n", expected_page.len())));
        assert!(received.contains(expected_page), "{received}");
        assert!(received.contains(&format!("Content-Security-Policy: {CSP}\r\n")));
        assert!(!received.to_lowercase().contains("alt-svc"));
        assert!(!received.contains("script-src 'self'"));
        assert!(
            received.contains("content-length: 11\r\ncache-control: no-store\r\n\r\ntizentube()")
        );
        assert!(received.ends_with("Content-Length: 4\r\n\r\n[42]"));
        assert_eq!(pages, 1);
    }

    #[test]
    fn passes_other_responses_through_with_their_framing() {
        let (client, to_client) =
            duplex("GET /tv_config HTTP/1.1\r\nHost: www.youtube.com\r\n\r\n");
        let response =
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\n\r\n\
                        3;x=y\r\nabc\r\n0\r\nTrailer: 1\r\n\r\n";
        let (upstream, _) = duplex(response);
        let mut upstream = Some(upstream);
        let script = Script {
            version: "1".into(),
            body: Vec::new(),
        };
        relay(
            client,
            || upstream.take().ok_or(io::ErrorKind::NotConnected.into()),
            Some(&script),
            || panic!("not the TV page"),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(to_client.borrow().clone()).unwrap(),
            response
        );
    }

    #[test]
    fn reads_long_header_lines() {
        let long = "x".repeat(10_000);
        let text = format!("HTTP/1.1 200 OK\r\nContent-Security-Policy: {long}\r\n\r\n");
        let head = read_head(&mut Cursor::new(text.into_bytes()))
            .unwrap()
            .unwrap();
        assert_eq!(
            head.get("content-security-policy").map(str::len),
            Some(10_000)
        );
        let huge = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "y".repeat(MAX_HEAD));
        assert!(read_head(&mut Cursor::new(huge.into_bytes())).is_err());
    }

    #[test]
    fn tv_page_matching() {
        assert!(is_tv_page("/tv"));
        assert!(is_tv_page("/tv?x=1"));
        assert!(!is_tv_page("/tv_config"));
        assert!(!is_tv_page("/tvx"));
        assert_eq!(rewrite_page("<p>no body", "<s>"), "<p>no body<s>");
    }
}
