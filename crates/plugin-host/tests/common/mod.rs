//! Shared fixtures: tarballs, a registry index, and a small HTTP server.

#![allow(dead_code, unreachable_pub)]

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anya_plugin_host::integrity_of;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

/// An npm tarball (`package/` prefix) of `files`.
pub fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        tar.append_data(&mut header, format!("package/{name}"), *bytes)
            .expect("append");
    }
    gzip(tar.into_inner().expect("tar"))
}

/// A tarball with one entry whose raw path is `name`, however unsafe.
pub fn tgz_raw(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
    header.set_cksum();
    let mut tar = Vec::new();
    tar.extend_from_slice(header.as_bytes());
    tar.extend_from_slice(bytes);
    tar.resize(tar.len().div_ceil(512) * 512 + 1024, 0);
    gzip(tar)
}

fn gzip(tar: Vec<u8>) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).expect("gzip");
    gz.finish().expect("gzip")
}

/// A tarball of the files under `dir`, paths relative to it.
pub fn tgz_of_dir(dir: &Path) -> Vec<u8> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let name = path.strip_prefix(base).expect("prefix");
                out.push((
                    name.display().to_string(),
                    std::fs::read(&path).expect("read"),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    let files: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    tgz(&files)
}

/// The example engine under `sdk/plugin/examples`.
pub fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sdk/plugin/examples")
        .join(name)
}

/// An index with one plugin and one release of it.
pub fn index(id: &str, version: &str, artifact: &str, tgz: &[u8]) -> Value {
    json!({
        "version": 1,
        "plugins": [{
            "id": id,
            "name": id,
            "versions": [release(version, artifact, tgz)]
        }]
    })
}

/// One release entry for `tgz`, compatible with every Anya.
pub fn release(version: &str, artifact: &str, tgz: &[u8]) -> Value {
    json!({
        "version": version,
        "artifact": artifact,
        "integrity": integrity_of(tgz),
        "anya": ">=0.0.0",
        "protocol": 1,
        "kind": "engine",
        "streaming": true,
        "permissions": []
    })
}

/// Serves `files` (path to body) on a loopback port; returns the base URL.
pub async fn serve(files: HashMap<String, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let files = Arc::new(files);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let files = Arc::clone(&files);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&request);
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let response = match files.get(&path) {
                    Some(body) => {
                        let mut out = format!(
                            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        out.extend_from_slice(body);
                        out
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}
