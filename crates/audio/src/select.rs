//! Which listed device a configured value names.

use anya_core::audio::{AudioError, Device};

/// The index of the device `wanted` names: the first exact id, otherwise
/// the first name equal ignoring case. Anything else is
/// [`AudioError::NotFound`], never another device.
pub(crate) fn find(devices: &[Device], wanted: &str) -> Result<usize, AudioError> {
    let wanted = wanted.trim();
    if let Some(i) = devices.iter().position(|d| d.id == wanted) {
        return Ok(i);
    }
    let lower = wanted.to_lowercase();
    if let Some(i) = devices.iter().position(|d| d.name.to_lowercase() == lower) {
        return Ok(i);
    }
    Err(not_found(wanted, devices))
}

/// `wanted` is not among `devices`.
pub(crate) fn not_found(wanted: &str, devices: &[Device]) -> AudioError {
    AudioError::NotFound {
        wanted: wanted.to_owned(),
        available: devices.iter().map(|d| d.name.clone()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use anya_core::audio::DeviceKind;

    use super::*;

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: DeviceKind::Microphone,
        }
    }

    fn devices() -> Vec<Device> {
        vec![
            device("BuiltInMicrophoneDevice", "MacBook Pro Microphone"),
            device("BlackHole2ch_UID", "BlackHole 2ch"),
            device("usb-mic", "麦克风 USB"),
        ]
    }

    #[test]
    fn an_exact_id_matches() {
        assert_eq!(find(&devices(), "BlackHole2ch_UID").unwrap(), 1);
    }

    #[test]
    fn a_name_matches_ignoring_case() {
        assert_eq!(find(&devices(), "BlackHole 2ch").unwrap(), 1);
        assert_eq!(find(&devices(), "blackhole 2CH").unwrap(), 1);
        assert_eq!(find(&devices(), " macbook pro microphone ").unwrap(), 0);
        assert_eq!(find(&devices(), "麦克风 usb").unwrap(), 2);
    }

    #[test]
    fn ids_are_case_sensitive() {
        let err = find(&devices(), "blackhole2ch_uid").unwrap_err();
        assert!(matches!(err, AudioError::NotFound { .. }));
    }

    #[test]
    fn an_id_wins_over_an_earlier_name() {
        let list = vec![device("a", "studio"), device("studio", "Desk Mic")];
        assert_eq!(find(&list, "studio").unwrap(), 1);
    }

    #[test]
    fn an_unknown_device_lists_what_is_connected() {
        match find(&devices(), "Yeti") {
            Err(AudioError::NotFound { wanted, available }) => {
                assert_eq!(wanted, "Yeti");
                assert_eq!(
                    available,
                    ["MacBook Pro Microphone", "BlackHole 2ch", "麦克风 USB"]
                );
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
        assert!(matches!(
            find(&[], "default"),
            Err(AudioError::NotFound { available, .. }) if available.is_empty()
        ));
    }
}
