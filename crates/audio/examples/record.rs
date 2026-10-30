//! Lists input devices, runs the audio doctor checks, then records 3 s from
//! the default input (the device named by the first argument, or with
//! `--system` what the computer plays) and prints the chunk count and peak
//! level.
//!
//! `cargo run -p anya-audio --example record [device | --system]`

#![allow(clippy::print_stdout)]

use std::time::Instant;

use anya_audio::System;
use anya_core::audio::{AudioSource, CHUNK_BYTES, CHUNK_MS, DeviceKind};
use anya_core::desktop::Desktop;

fn main() {
    let source = System::new(1);
    match source.devices() {
        Ok(devices) => {
            println!("input devices:");
            for d in &devices {
                println!("  {:?}  {}  [{}]", d.kind, d.name, d.id);
            }
        }
        Err(e) => println!("cannot list devices: {e}"),
    }
    for check in anya_audio::checks(Desktop::detect()) {
        println!(
            "check {}: ok={} outcome={} {:?}",
            check.code, check.ok, check.outcome, check.values
        );
    }

    let (wanted, default) = match std::env::args().nth(1) {
        Some(arg) if arg == "--system" => (None, DeviceKind::Loopback),
        other => (other, DeviceKind::Microphone),
    };
    let capture = match source.open(wanted.as_deref(), default) {
        Ok(capture) => capture,
        Err(e) => {
            println!("open failed: {e}");
            std::process::exit(1);
        }
    };
    let mut chunks = capture.chunks;
    let want = 3000 / CHUNK_MS as usize;
    let started = Instant::now();
    let (mut count, mut peak_sample, mut peak_level) = (0usize, 0i32, 0f32);
    while count < want {
        match chunks.blocking_recv() {
            Some(Ok(chunk)) => {
                assert_eq!(chunk.0.len(), CHUNK_BYTES);
                count += 1;
                peak_level = peak_level.max(chunk.level());
                for b in chunk.0.chunks_exact(2) {
                    let s = i32::from(i16::from_le_bytes([b[0], b[1]])).abs();
                    peak_sample = peak_sample.max(s);
                }
            }
            Some(Err(e)) => {
                println!("capture failed: {e}");
                break;
            }
            None => break,
        }
    }
    let elapsed = started.elapsed();
    drop(chunks);
    let dbfs = if peak_sample == 0 {
        f64::NEG_INFINITY
    } else {
        20.0 * (f64::from(peak_sample) / 32768.0).log10()
    };
    println!(
        "recorded {count} chunks of {CHUNK_BYTES} bytes in {elapsed:.2?}; peak sample {peak_sample} ({dbfs:.1} dBFS), peak level {peak_level:.2}"
    );
}
