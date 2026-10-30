//! SIGINT and SIGTERM end `anya subscribe` with reason `signal`. A binary of
//! its own: the test signals its own process.

mod common;

use std::time::Duration;

use anya_protocol::{Source, SourceState};
use anya_server::{ExitReason, Server, parse_address, subscribe};
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signal_ends_the_run() {
    let hub = hub();
    hub.set_source(Source::Continuous, SourceState::Listening, None);
    let server = Server::start(
        hub,
        parse_address("127.0.0.1:0").expect("address"),
        None,
        None,
    )
    .await
    .expect("start");
    for signal in ["-INT", "-TERM"] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let send = async {
            // Connected means the handlers are in place: subscribe registers
            // them before it dials.
            eventually(|| server.subscribers() == 1).await;
            let status = tokio::process::Command::new("kill")
                .arg(signal)
                .arg(std::process::id().to_string())
                .status()
                .await
                .expect("kill");
            assert!(status.success());
        };
        let (exit, ()) = tokio::time::timeout(WAIT, async {
            tokio::join!(
                subscribe(address_of(&server), None, None, &mut out, &mut err),
                send
            )
        })
        .await
        .expect("the signal ended the run");
        let exit = exit.expect("connected");
        assert_eq!(exit.reason, ExitReason::Signal, "{signal}");
        assert!(exit.connected);
        let stderr = String::from_utf8(err).expect("UTF-8");
        assert!(
            stderr.ends_with(" (reason: signal)\n"),
            "{signal}: {stderr}"
        );
        eventually(|| server.subscribers() == 0).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server.stop().await;
}
