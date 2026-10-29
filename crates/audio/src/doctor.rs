//! Whether this machine can record: the doctor's audio checks.

use anya_core::desktop::Desktop;
use anya_core::doctor::{Check, Severity};
use cpal::ErrorKind;
use cpal::traits::{DeviceTrait, HostTrait};

/// Code of the check that a default input device opens.
const AUDIO_INPUT: &str = "audio_input";

/// Code of the macOS microphone permission check.
#[cfg(target_os = "macos")]
const MICROPHONE_PERMISSION: &str = "microphone_permission";

/// The checks capture needs, cheapest first. Capture is the same on every
/// desktop of a platform, so `desktop` changes nothing today.
///
/// - `microphone_permission` (macOS, required): `granted`, `not_determined`
///   (passes; macOS asks on the first recording), `denied`, `restricted`.
///   Never prompts.
/// - `audio_input` (required): `available` with `device`, `none`, `denied`
///   or `failed` with `detail`. Reads the default input's format and closes
///   it again; nothing is recorded.
pub fn checks(desktop: Desktop) -> Vec<Check> {
    let _ = desktop;
    #[cfg(target_os = "macos")]
    let checks = vec![
        permission_check(crate::macos::permission()),
        input_check(probe()),
    ];
    #[cfg(not(target_os = "macos"))]
    let checks = vec![input_check(probe())];
    checks
}

/// What opening the default input found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Probe {
    Available(String),
    None,
    Denied(String),
    Failed(String),
}

fn probe() -> Probe {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return Probe::None;
    };
    let name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "default".into());
    match device.default_input_config() {
        Ok(_) => Probe::Available(name),
        Err(e) => match e.kind() {
            ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable => Probe::None,
            ErrorKind::PermissionDenied => Probe::Denied(e.to_string()),
            _ => Probe::Failed(e.to_string()),
        },
    }
}

fn input_check(probe: Probe) -> Check {
    let severity = Severity::Required;
    match probe {
        Probe::Available(name) => {
            Check::passed(AUDIO_INPUT, severity, "available").with("device", name)
        }
        Probe::None => Check::failed(AUDIO_INPUT, severity, "none"),
        Probe::Denied(detail) => {
            Check::failed(AUDIO_INPUT, severity, "denied").with("detail", detail)
        }
        Probe::Failed(detail) => {
            Check::failed(AUDIO_INPUT, severity, "failed").with("detail", detail)
        }
    }
}

#[cfg(target_os = "macos")]
fn permission_check(permission: crate::macos::Permission) -> Check {
    use crate::macos::Permission;

    let severity = Severity::Required;
    match permission {
        Permission::Granted => Check::passed(MICROPHONE_PERMISSION, severity, "granted"),
        Permission::NotDetermined => {
            Check::passed(MICROPHONE_PERMISSION, severity, "not_determined")
        }
        Permission::Denied => Check::failed(MICROPHONE_PERMISSION, severity, "denied"),
        Permission::Restricted => Check::failed(MICROPHONE_PERMISSION, severity, "restricted"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_input_that_opens_passes_with_its_name() {
        let check = input_check(Probe::Available("MacBook Pro Microphone".into()));
        assert!(check.ok);
        assert_eq!(check.code, AUDIO_INPUT);
        assert_eq!(check.outcome, "available");
        assert_eq!(
            check.values,
            [("device", "MacBook Pro Microphone".to_owned())]
        );
    }

    #[test]
    fn no_input_or_a_refusal_blocks_startup() {
        let none = input_check(Probe::None);
        assert!(none.blocks());
        assert_eq!(none.outcome, "none");
        let denied = input_check(Probe::Denied("EACCES".into()));
        assert!(denied.blocks());
        assert_eq!(denied.outcome, "denied");
        assert_eq!(denied.values, [("detail", "EACCES".to_owned())]);
        let failed = input_check(Probe::Failed("busy".into()));
        assert_eq!(failed.outcome, "failed");
        assert!(failed.blocks());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn microphone_permission_outcomes() {
        use crate::macos::Permission;

        let cases = [
            (Permission::Granted, true, "granted"),
            (Permission::NotDetermined, true, "not_determined"),
            (Permission::Denied, false, "denied"),
            (Permission::Restricted, false, "restricted"),
        ];
        for (permission, ok, outcome) in cases {
            let check = permission_check(permission);
            assert_eq!(check.code, MICROPHONE_PERMISSION);
            assert_eq!((check.ok, check.outcome), (ok, outcome));
            assert_eq!(check.severity, Severity::Required);
        }
    }
}
