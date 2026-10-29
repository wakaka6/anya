//! Who may ask: the per-run key, the cookie that keeps it, `Host` and
//! `Origin` (see the module docs of [`super`]).

use hyper::header::{self, HeaderMap};
use hyper::{Method, Request, StatusCode};
use subtle::ConstantTimeEq;

use super::reply::{self, Reply};
use super::{Page, WebError};

/// A fresh key: 16 random bytes, hex.
pub(super) fn new_key() -> Result<String, WebError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| WebError::Random(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `None` lets the request through; otherwise the answer to send instead.
pub(super) fn guard<B>(page: &Page, req: &Request<B>) -> Option<Reply> {
    let headers = req.headers();
    let refuse = |status, key: &str| Some(reply::text(status, page.catalog().t(key)));
    // A page on another site can resolve its own name to 127.0.0.1; only this
    // address is ours.
    if header_str(headers, header::HOST) != Some(page.host.as_str()) {
        return refuse(StatusCode::FORBIDDEN, "gui.web.forbidden");
    }
    if req.method() != Method::GET
        && header_str(headers, header::ORIGIN) != Some(format!("http://{}", page.host).as_str())
    {
        return refuse(StatusCode::FORBIDDEN, "gui.web.forbidden");
    }
    if let Some(key) = query_key(req.uri().query()) {
        if !same(&key, &page.key) {
            return refuse(StatusCode::UNAUTHORIZED, "gui.web.link_expired");
        }
        // The path parsed as a URI; `//host` would leave the origin.
        let path = match req.uri().path() {
            p if p.starts_with('/') && !p.starts_with("//") => p,
            _ => "/",
        };
        let cookie = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict",
            page.cookie, page.key
        );
        return Some(reply::sign_in(path, &cookie));
    }
    match cookie(headers, &page.cookie) {
        Some(value) if same(value, &page.key) => None,
        _ => refuse(StatusCode::UNAUTHORIZED, "gui.web.sign_in"),
    }
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The `key` query parameter, percent-decoded; an empty one counts as absent.
fn query_key(query: Option<&str>) -> Option<String> {
    query?
        .split('&')
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .find(|(name, _)| *name == "key")
        .map(|(_, value)| percent_decode(value))
        .filter(|v| !v.is_empty())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The value of cookie `name` in any `Cookie` header.
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
}

/// Equal, in time that does not depend on where they differ.
fn same(given: &str, key: &str) -> bool {
    given.as_bytes().ct_eq(key.as_bytes()).into()
}
