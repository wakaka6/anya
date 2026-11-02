//! Responses: every one carries the same protective headers.

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{self, HeaderValue};
use hyper::{Response, StatusCode};

/// One response.
pub(super) type Reply = Response<Full<Bytes>>;

/// Scripts, styles and requests come only from the page itself; nothing may
/// frame it.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
                   img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; \
                   base-uri 'none'; form-action 'none'";

fn base(status: StatusCode, content_type: &'static str, cache: &'static str, body: Bytes) -> Reply {
    let mut res = Response::new(Full::new(body));
    *res.status_mut() = status;
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // The sign-in link carries the key; never pass it on.
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    res
}

/// `value` as JSON.
pub(super) fn json(status: StatusCode, value: &serde_json::Value) -> Reply {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    base(
        status,
        "application/json; charset=utf-8",
        "no-store",
        body.into(),
    )
}

/// `{"error": message}`.
pub(super) fn error(status: StatusCode, message: &str) -> Reply {
    json(status, &serde_json::json!({ "error": message }))
}

/// Plain text: refusals before the page is reached.
pub(super) fn text(status: StatusCode, message: &str) -> Reply {
    base(
        status,
        "text/plain; charset=utf-8",
        "no-store",
        Bytes::copy_from_slice(message.as_bytes()),
    )
}

/// 204.
pub(super) fn empty() -> Reply {
    base(
        StatusCode::NO_CONTENT,
        "text/plain; charset=utf-8",
        "no-store",
        Bytes::new(),
    )
}

/// An embedded file. Revalidated, so an upgraded Anya never runs an old
/// script against a new API.
pub(super) fn asset(content_type: &'static str, body: &'static [u8]) -> Reply {
    base(
        StatusCode::OK,
        content_type,
        "no-cache",
        Bytes::from_static(body),
    )
}

/// 303 to `location`, setting `cookie`.
pub(super) fn sign_in(location: &str, cookie: &str) -> Reply {
    let mut res = base(
        StatusCode::SEE_OTHER,
        "text/plain; charset=utf-8",
        "no-store",
        Bytes::new(),
    );
    let h = res.headers_mut();
    // Both are built from the key (hex), the port and a request path that
    // already parsed as a URI, so they are valid header values.
    if let Ok(v) = HeaderValue::from_str(location) {
        h.insert(header::LOCATION, v);
    }
    if let Ok(v) = HeaderValue::from_str(cookie) {
        h.insert(header::SET_COOKIE, v);
    }
    res
}

/// An engine's or a plugin's icon. Its name is its content's hash, so it never changes; an
/// SVG opened on its own is only ever drawn, never run.
pub(super) fn icon(content_type: &'static str, bytes: Vec<u8>) -> Reply {
    let mut res = base(
        StatusCode::OK,
        content_type,
        "private, max-age=31536000, immutable",
        bytes.into(),
    );
    res.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"),
    );
    res
}
