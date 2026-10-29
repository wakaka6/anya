//! Which addresses the server listens on and the client connects to.

use std::net::SocketAddr;
use std::path::PathBuf;

use anya_server::{Address, AddressError, DEFAULT_ADDRESS, parse_address};

fn tcp(s: &str) -> Address {
    Address::Tcp(s.parse::<SocketAddr>().expect("socket address"))
}

#[test]
fn loopback_host_port_is_accepted() {
    for (given, parsed) in [
        ("127.0.0.1:7590", "127.0.0.1:7590"),
        ("127.0.0.2:7590", "127.0.0.2:7590"),
        ("[::1]:7590", "[::1]:7590"),
        ("localhost:7590", "127.0.0.1:7590"),
        ("LocalHost:7590", "127.0.0.1:7590"),
        ("127.0.0.1:0", "127.0.0.1:0"),
        ("  127.0.0.1:7590 ", "127.0.0.1:7590"),
    ] {
        assert_eq!(parse_address(given), Ok(tcp(parsed)), "{given}");
    }
    assert_eq!(parse_address(DEFAULT_ADDRESS), Ok(tcp("127.0.0.1:7590")));
}

#[test]
fn anything_off_loopback_is_refused() {
    for given in [
        "0.0.0.0:7590",
        "[::]:7590",
        "192.168.1.2:7590",
        "example.com:7590",
        ":7590",
    ] {
        assert!(
            matches!(parse_address(given), Err(AddressError::NotLoopback(_))),
            "{given}: {:?}",
            parse_address(given)
        );
    }
}

#[test]
fn malformed_addresses_are_refused() {
    for given in [
        "127.0.0.1",
        "7590",
        "localhost",
        "127.0.0.1:port",
        "127.0.0.1:70000",
        "ws://127.0.0.1:7590/asr",
        "::1:7590",
    ] {
        assert!(
            matches!(parse_address(given), Err(AddressError::Invalid { .. })),
            "{given}: {:?}",
            parse_address(given)
        );
    }
}

#[test]
fn empty_is_an_error_the_caller_replaces_with_the_default() {
    assert_eq!(parse_address(""), Err(AddressError::Empty));
    assert_eq!(parse_address(" \t"), Err(AddressError::Empty));
}

#[test]
fn unix_paths_expand_home() {
    assert_eq!(
        parse_address("unix:/run/user/1000/anya.sock"),
        Ok(Address::Unix(PathBuf::from("/run/user/1000/anya.sock")))
    );
    assert_eq!(
        parse_address("unix: relative.sock "),
        Ok(Address::Unix(PathBuf::from("relative.sock")))
    );
    assert!(matches!(
        parse_address("unix:"),
        Err(AddressError::MissingPath(_))
    ));
    assert!(matches!(
        parse_address("unix:   "),
        Err(AddressError::MissingPath(_))
    ));
    assert_eq!(
        parse_address("unix:~other/a.sock"),
        Ok(Address::Unix(PathBuf::from("~other/a.sock")))
    );
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set for tests"));
    assert_eq!(
        parse_address("unix:~/.anya/a.sock"),
        Ok(Address::Unix(home.join(".anya/a.sock")))
    );
    assert_eq!(parse_address("unix:~"), Ok(Address::Unix(home)));
}

#[test]
fn addresses_print_as_subscribers_connect_to_them() {
    let tcp = parse_address("localhost:7590").expect("tcp");
    assert_eq!(tcp.url(), "ws://127.0.0.1:7590/asr");
    assert_eq!(tcp.to_string(), "127.0.0.1:7590");
    let unix = parse_address("unix:/tmp/a.sock").expect("unix");
    assert_eq!(unix.url(), "unix:/tmp/a.sock");
    assert_eq!(unix.to_string(), "unix:/tmp/a.sock");
}
