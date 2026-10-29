//! Walks the overlay through a session: idle, listening with a rising level
//! and growing text, finishing, done, an error, idle again.
//!
//! ```sh
//! cargo run -p anya-overlay --example demo [position] [scale] [idle-visible]
//! ```
//!
//! The example is its own helper: the overlay starts it again with
//! `HELPER_ARG`.

use std::thread::sleep;
use std::time::Duration;

use anya_core::desktop::Desktop;
use anya_overlay::{HELPER_ARG, Labels, Overlay, Position, Style, View};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "macos")]
    if args.get(1).map(String::as_str) == Some(HELPER_ARG) {
        anya_overlay::helper_main();
    }
    let _ = HELPER_ARG;

    let position = match args.get(1) {
        Some(p) => p.parse()?,
        None => Position::platform_default(),
    };
    let scale = match args.get(2) {
        Some(s) => s.parse()?,
        None => 1.0,
    };
    let idle_visible = args.get(3).is_some_and(|v| v == "true");
    let overlay = Overlay::start(
        Desktop::detect(),
        Style {
            position,
            scale,
            idle_visible,
            show_text: true,
        },
        Labels {
            listening: "正在聆听…".into(),
            recognizing: "正在识别…".into(),
            idle: "Anya".into(),
        },
    )?;
    let step = |ms| sleep(Duration::from_millis(ms));

    overlay.show(View::Idle);
    step(800);

    let words = "今天下午三点 我们在会议室 讨论一下 新版本的发布计划 \
                 顺便看看 overlay 在刘海下面 显示得怎么样 文字太长的时候 \
                 应该只保留最新的两行";
    let parts: Vec<&str> = words.split(' ').collect();
    let mut text = String::new();
    for frame in 0..200u32 {
        let t = f64::from(frame) * 0.04;
        let level = ((t * 0.25).min(1.0) * (0.55 + 0.45 * (t * 6.0).sin().abs())) as f32;
        if frame >= 25
            && frame % 15 == 0
            && let Some(part) = parts.get((frame as usize - 25) / 15)
        {
            text.push_str(part);
        }
        overlay.show(View::Listening {
            level,
            text: text.clone(),
        });
        step(40);
    }

    overlay.show(View::Finishing { text: text.clone() });
    step(1500);
    overlay.show(View::Done {
        text,
        label: "已粘贴".into(),
    });
    step(2500);

    overlay.show(View::Listening {
        level: 0.3,
        text: String::new(),
    });
    step(1200);
    overlay.show(View::Error {
        message: "认证失败，请检查 App Key 或 Access Key。".into(),
        hint: "按 R 重试，Esc 关闭".into(),
    });
    step(3000);
    overlay.show(View::Idle);
    step(1200);
    Ok(())
}
