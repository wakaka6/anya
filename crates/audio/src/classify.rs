//! What a device records, which devices are listed, and how they are named.
//! Pure rules over what the platform reports, so both platforms test them.

use anya_core::audio::{Device, DeviceKind};

/// CoreAudio's `kAudioDeviceTransportTypeVirtual`.
#[cfg(any(target_os = "macos", test))]
const TRANSPORT_VIRTUAL: u32 = u32::from_be_bytes(*b"virt");

/// CoreAudio's `kAudioDeviceTransportTypeAggregate`.
#[cfg(any(target_os = "macos", test))]
const TRANSPORT_AGGREGATE: u32 = u32::from_be_bytes(*b"grup");

/// A CoreAudio input is a loopback when it is virtual or aggregate and also
/// plays (BlackHole, Loopback, an aggregate with a speaker); every other
/// input, or one whose transport cannot be read, records the room.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn coreaudio_kind(transport: Option<u32>, has_output: bool) -> DeviceKind {
    match transport {
        Some(TRANSPORT_VIRTUAL | TRANSPORT_AGGREGATE) if has_output => DeviceKind::Loopback,
        _ => DeviceKind::Microphone,
    }
}

/// An ALSA PCM records what the computer plays when it is a PulseAudio or
/// PipeWire monitor source (`….monitor`, "Monitor of …") or a card of the
/// `snd-aloop` loopback driver.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn alsa_kind(id: &str, description: &str) -> DeviceKind {
    let id = id.to_lowercase();
    let description = description.to_lowercase();
    let monitor = id.contains(".monitor")
        || description.contains(".monitor")
        || description
            .lines()
            .any(|line| line.trim_start().starts_with("monitor of "));
    if monitor || id.contains("card=loopback") {
        DeviceKind::Loopback
    } else {
        DeviceKind::Microphone
    }
}

/// ALSA plugins that only play or remap a card's output channels.
#[cfg(any(target_os = "linux", test))]
const PLAYBACK_ONLY: &[&str] = &[
    "front",
    "rear",
    "center_lfe",
    "side",
    "surround21",
    "surround40",
    "surround41",
    "surround50",
    "surround51",
    "surround71",
    "iec958",
    "spdif",
    "hdmi",
    "dmix",
    "usbstream",
];

/// Which ALSA PCMs, given as `(id, first description line)`, are worth
/// listing as inputs: not `null`, not a playback-only plugin, and not the
/// numbered `hw:CARD=0,…` twin cpal adds beside a card's named hint.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn alsa_listed(pcms: &[(&str, &str)]) -> Vec<bool> {
    pcms.iter()
        .map(|&(id, title)| {
            if id == "null" {
                return false;
            }
            let (prefix, args) = id.split_once(':').unwrap_or((id, ""));
            if PLAYBACK_ONLY.contains(&prefix) {
                return false;
            }
            if numbered_card(prefix, args) {
                return !pcms.iter().any(|&(other, other_title)| {
                    other_title == title
                        && other
                            .split_once(':')
                            .is_some_and(|(p, a)| p == prefix && !numbered_card(p, a))
                });
            }
            true
        })
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn numbered_card(prefix: &str, args: &str) -> bool {
    matches!(prefix, "hw" | "plughw")
        && args.split(',').any(|arg| {
            arg.strip_prefix("CARD=")
                .is_some_and(|card| !card.is_empty() && card.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Builds the device list from `(id, name, kind)`. Devices whose names are
/// equal ignoring case get their id appended, so a name picks one device.
pub(crate) fn named(entries: Vec<(String, String, DeviceKind)>) -> Vec<Device> {
    let lower: Vec<String> = entries.iter().map(|(_, n, _)| n.to_lowercase()).collect();
    entries
        .into_iter()
        .zip(&lower)
        .map(|((id, name, kind), key)| {
            let shared = lower.iter().filter(|other| *other == key).count() > 1;
            let name = if shared {
                format!("{name} ({id})")
            } else {
                name
            };
            Device { id, name, kind }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILT_IN: u32 = u32::from_be_bytes(*b"bltn");
    const USB: u32 = u32::from_be_bytes(*b"usb ");

    #[test]
    fn coreaudio_virtual_or_aggregate_with_output_is_loopback() {
        assert_eq!(
            coreaudio_kind(Some(TRANSPORT_VIRTUAL), true),
            DeviceKind::Loopback
        );
        assert_eq!(
            coreaudio_kind(Some(TRANSPORT_AGGREGATE), true),
            DeviceKind::Loopback
        );
    }

    #[test]
    fn coreaudio_everything_else_is_a_microphone() {
        assert_eq!(
            coreaudio_kind(Some(TRANSPORT_VIRTUAL), false),
            DeviceKind::Microphone
        );
        assert_eq!(
            coreaudio_kind(Some(TRANSPORT_AGGREGATE), false),
            DeviceKind::Microphone
        );
        assert_eq!(coreaudio_kind(Some(BUILT_IN), true), DeviceKind::Microphone);
        assert_eq!(coreaudio_kind(Some(USB), false), DeviceKind::Microphone);
        assert_eq!(coreaudio_kind(None, true), DeviceKind::Microphone);
    }

    #[test]
    fn alsa_monitors_and_aloop_are_loopback() {
        let loopback = [
            ("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor", ""),
            ("pulse", "Monitor of Built-in Audio Analog Stereo"),
            (
                "meeting",
                "PipeWire source\nalsa_output.usb-headset.MONITOR",
            ),
            ("hw:CARD=Loopback,DEV=1", "Loopback, Loopback PCM"),
        ];
        for (id, description) in loopback {
            assert_eq!(alsa_kind(id, description), DeviceKind::Loopback, "{id}");
        }
    }

    #[test]
    fn alsa_cards_and_servers_are_microphones() {
        let mics = [
            (
                "default",
                "Default ALSA Output (currently PulseAudio Sound Server)",
            ),
            ("pipewire", "PipeWire Sound Server"),
            ("hw:CARD=PCH,DEV=0", "HDA Intel PCH, ALC892 Analog"),
            ("plughw:CARD=1,DEV=0", "USB Audio, USB Audio"),
        ];
        for (id, description) in mics {
            assert_eq!(alsa_kind(id, description), DeviceKind::Microphone, "{id}");
        }
    }

    #[test]
    fn alsa_listing_drops_null_playback_plugins_and_numbered_twins() {
        let analog = "HDA Intel PCH, ALC892 Analog";
        let pcms = [
            ("null", "Discard all samples"),
            ("default", "Default ALSA Output"),
            ("pipewire", "PipeWire Sound Server"),
            ("front:CARD=PCH,DEV=0", analog),
            ("dmix:CARD=PCH,DEV=0", analog),
            ("hw:CARD=PCH,DEV=0", analog),
            ("plughw:CARD=PCH,DEV=0", analog),
            ("dsnoop:CARD=PCH,DEV=0", analog),
            ("hw:CARD=0,DEV=0", analog),
            ("plughw:CARD=0,DEV=0", analog),
            ("hw:CARD=2,DEV=0", "Yeti, USB Audio"),
        ];
        let kept: Vec<&str> = pcms
            .iter()
            .zip(alsa_listed(&pcms))
            .filter(|(_, keep)| *keep)
            .map(|((id, _), _)| *id)
            .collect();
        assert_eq!(
            kept,
            [
                "default",
                "pipewire",
                "hw:CARD=PCH,DEV=0",
                "plughw:CARD=PCH,DEV=0",
                "dsnoop:CARD=PCH,DEV=0",
                "hw:CARD=2,DEV=0",
            ]
        );
    }

    #[test]
    fn shared_names_get_their_id() {
        let mic = DeviceKind::Microphone;
        let list = named(vec![
            ("hw:CARD=PCH,DEV=0".into(), "HDA Intel PCH".into(), mic),
            ("plughw:CARD=PCH,DEV=0".into(), "hda intel pch".into(), mic),
            ("default".into(), "Default".into(), mic),
        ]);
        let names: Vec<&str> = list.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "HDA Intel PCH (hw:CARD=PCH,DEV=0)",
                "hda intel pch (plughw:CARD=PCH,DEV=0)",
                "Default",
            ]
        );
        assert_eq!(list[1].id, "plughw:CARD=PCH,DEV=0");
    }
}
