//! What cpal does not report on macOS: what each input records, and whether
//! Anya may use the microphone.

use std::collections::HashMap;
use std::ptr::{NonNull, null};
use std::sync::mpsc;

use anya_core::audio::{AudioError, DeviceKind};
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyStreams, kAudioDevicePropertyTransportType,
    kAudioHardwareNoError, kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
};
use objc2_core_foundation::{CFRetained, CFString};

use crate::classify;

fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// The size in bytes of a property, if the object has it.
fn size(object: AudioObjectID, address: &AudioObjectPropertyAddress) -> Option<u32> {
    let mut size = 0u32;
    // SAFETY: `address` and `size` point to live values for the whole call
    // and no qualifier is passed.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(address),
            0,
            null(),
            NonNull::from(&mut size),
        )
    };
    (status == kAudioHardwareNoError).then_some(size)
}

/// A `UInt32` property.
fn read_u32(object: AudioObjectID, address: &AudioObjectPropertyAddress) -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` has room for the `size` bytes CoreAudio may write, and
    // every bit pattern is a valid u32.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == kAudioHardwareNoError && size as usize == size_of::<u32>()).then_some(value)
}

/// Every audio device's object id.
fn device_ids() -> Option<Vec<AudioObjectID>> {
    let system = kAudioObjectSystemObject as AudioObjectID;
    let address = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );
    let bytes = size(system, &address)?;
    let mut ids: Vec<AudioObjectID> = vec![0; bytes as usize / size_of::<AudioObjectID>()];
    let mut size = (ids.len() * size_of::<AudioObjectID>()) as u32;
    // SAFETY: `ids` has room for `size` bytes of AudioObjectIDs, the type this
    // property holds; any bit pattern is a valid id.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new(ids.as_mut_ptr())?.cast(),
        )
    };
    if status != kAudioHardwareNoError {
        return None;
    }
    ids.truncate(size as usize / size_of::<AudioObjectID>());
    Some(ids)
}

/// A device's UID, the id cpal reports for it.
fn uid(device: AudioObjectID) -> Option<String> {
    let address = address(
        kAudioDevicePropertyDeviceUID,
        kAudioObjectPropertyScopeGlobal,
    );
    let mut uid: *mut CFString = std::ptr::null_mut();
    let mut size = size_of::<*mut CFString>() as u32;
    // SAFETY: the property is a CFStringRef; `uid` has room for one pointer.
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::from(&mut uid).cast(),
        )
    };
    if status != kAudioHardwareNoError {
        return None;
    }
    // SAFETY: CoreAudio returns the UID retained (the Copy rule), so taking
    // ownership releases it exactly once.
    let uid = unsafe { CFRetained::from_raw(NonNull::new(uid)?) };
    Some(uid.to_string())
}

/// What each device records, by UID. Empty when CoreAudio cannot list
/// devices; callers then treat every input as a microphone.
pub(crate) fn device_kinds() -> HashMap<String, DeviceKind> {
    let Some(ids) = device_ids() else {
        tracing::warn!("CoreAudio: cannot read the device list");
        return HashMap::new();
    };
    ids.into_iter()
        .filter_map(|id| {
            let uid = uid(id)?;
            let transport = read_u32(
                id,
                &address(
                    kAudioDevicePropertyTransportType,
                    kAudioObjectPropertyScopeGlobal,
                ),
            );
            let has_output = size(
                id,
                &address(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput),
            )
            .is_some_and(|bytes| bytes > 0);
            Some((uid, classify::coreaudio_kind(transport, has_output)))
        })
        .collect()
}

/// Whether macOS lets this process record from the microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Permission {
    /// The user allowed it.
    Granted,
    /// Not asked yet; macOS asks when recording starts.
    NotDetermined,
    /// The user refused it in System Settings.
    Denied,
    /// A profile or parental control forbids it.
    Restricted,
}

/// The current microphone permission. Never prompts.
pub(crate) fn permission() -> Permission {
    // SAFETY: AVMediaTypeAudio is an immutable framework constant.
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
        return Permission::NotDetermined;
    };
    // SAFETY: `audio` is AVMediaTypeAudio, one of the two media types the
    // method accepts.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
    if status == AVAuthorizationStatus::Authorized {
        Permission::Granted
    } else if status == AVAuthorizationStatus::Denied {
        Permission::Denied
    } else if status == AVAuthorizationStatus::Restricted {
        Permission::Restricted
    } else {
        Permission::NotDetermined
    }
}

/// Ok when the microphone may be used. When the user was never asked, asks
/// and waits for the answer: until it comes, macOS would deliver silence.
pub(crate) fn authorize() -> Result<(), AudioError> {
    match permission() {
        Permission::Granted => Ok(()),
        Permission::Denied => Err(AudioError::Denied(
            "microphone access is turned off in System Settings".into(),
        )),
        Permission::Restricted => Err(AudioError::Denied(
            "microphone access is restricted on this Mac".into(),
        )),
        Permission::NotDetermined => request(),
    }
}

fn request() -> Result<(), AudioError> {
    // SAFETY: AVMediaTypeAudio is an immutable framework constant.
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
        // Without the constant there is nothing to ask; CoreAudio asks itself.
        return Ok(());
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let handler = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    // SAFETY: `audio` is AVMediaTypeAudio; the block is retained by
    // AVFoundation until it has run, and only sends on a channel, which is
    // safe from the arbitrary queue it runs on.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &handler) };
    match rx.recv() {
        Ok(true) => Ok(()),
        Ok(false) => Err(AudioError::Denied(
            "microphone access was not granted".into(),
        )),
        Err(_) => Err(AudioError::Backend(
            "the microphone permission request was dropped unanswered".into(),
        )),
    }
}
