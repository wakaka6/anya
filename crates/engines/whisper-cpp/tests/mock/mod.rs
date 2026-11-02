//! A local HTTP/1.1 server for tests: one request per connection, each
//! recorded and answered from a script.

#![allow(dead_code)]

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// A request as the server received it.
#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

/// One scripted answer.
#[derive(Debug, Clone)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: String,
}

impl Reply {
    pub(crate) fn json(status: u16, body: serde_json::Value) -> Reply {
        Reply {
            status,
            body: body.to_string(),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            500 => "Internal Server Error",
            _ => "Status",
        };
        format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.status,
            self.body.len(),
            self.body
        )
        .into_bytes()
    }
}

/// A running server: its base URL (`http://127.0.0.1:<port>/v1`) and what
/// it was sent.
pub(crate) struct Server {
    pub(crate) base: String,
    pub(crate) requests: mpsc::UnboundedReceiver<Request>,
}

impl Server {
    pub(crate) async fn next(&mut self) -> Request {
        self.requests.recv().await.expect("a request")
    }
}

/// Answers requests with `replies` in order, repeating the last one.
pub(crate) async fn serve(replies: Vec<Reply>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local address");
    let (tx, requests) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut replies = replies.into_iter();
        let mut last = None;
        while let Ok((mut stream, _)) = listener.accept().await {
            let Some(request) = read_request(&mut stream).await else {
                continue;
            };
            let reply = replies.next().or(last.take()).expect("a scripted reply");
            let _ = tx.send(request);
            let _ = stream.write_all(&reply.bytes()).await;
            let _ = stream.shutdown().await;
            last = Some(reply);
        }
    });
    Server {
        base: format!("http://{addr}/v1"),
        requests,
    }
}

/// A base URL nothing listens on.
pub(crate) async fn unreachable() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local address");
    drop(listener);
    format!("http://{addr}/v1")
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        read_more(stream, &mut buf).await?;
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let mut body = buf.split_off(head_end + 4);
    if let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        while body.len() < len {
            read_more(stream, &mut body).await?;
        }
        body.truncate(len);
    } else if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        while !body.ends_with(b"0\r\n\r\n") {
            read_more(stream, &mut body).await?;
        }
        body = dechunk(&body);
    }
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

async fn read_more(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Option<()> {
    let mut chunk = [0u8; 16 * 1024];
    let n = stream.read(&mut chunk).await.ok()?;
    if n == 0 {
        return None;
    }
    buf.extend_from_slice(&chunk[..n]);
    Some(())
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(eol) = find(raw, b"\r\n") {
        let size = std::str::from_utf8(&raw[..eol])
            .ok()
            .and_then(|s| usize::from_str_radix(s.trim(), 16).ok())
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = eol + 2;
        out.extend_from_slice(&raw[start..start + size]);
        raw = &raw[start + size + 2..];
    }
    out
}

/// Where `needle` first occurs in `haystack`.
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
