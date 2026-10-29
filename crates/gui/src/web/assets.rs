//! The page's files, embedded in the binary.

/// Every embedded file: request path, content type, contents.
pub(super) const FILES: &[(&str, &str, &str)] = &[
    (
        "/",
        "text/html; charset=utf-8",
        include_str!("../../assets/index.html"),
    ),
    (
        "/index.html",
        "text/html; charset=utf-8",
        include_str!("../../assets/index.html"),
    ),
    (
        "/panel.html",
        "text/html; charset=utf-8",
        include_str!("../../assets/panel.html"),
    ),
    (
        "/app.css",
        "text/css; charset=utf-8",
        include_str!("../../assets/app.css"),
    ),
    (
        "/common.js",
        "text/javascript; charset=utf-8",
        include_str!("../../assets/common.js"),
    ),
    (
        "/app.js",
        "text/javascript; charset=utf-8",
        include_str!("../../assets/app.js"),
    ),
    (
        "/panel.js",
        "text/javascript; charset=utf-8",
        include_str!("../../assets/panel.js"),
    ),
    (
        "/icon.svg",
        "image/svg+xml",
        include_str!("../../assets/icon.svg"),
    ),
];

/// The file served at `path`: its content type and contents.
pub(super) fn find(path: &str) -> Option<(&'static str, &'static str)> {
    FILES
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, ty, body)| (*ty, *body))
}
