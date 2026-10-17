//! Microphone (capture endpoint) enumeration for the tray's Microphone menu.
//! Windows: Core Audio's `IMMDeviceEnumerator`, active capture endpoints, on a
//! short-lived thread of its own so COM's apartment never touches the async runtime's threads.
//! No audio is captured here (voice capture comes later).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub trait AudioInputs: Send + Sync {
    fn capture_devices(&self) -> Result<Vec<CaptureDevice>, String>;
}

pub struct NoAudio;
impl AudioInputs for NoAudio {
    fn capture_devices(&self) -> Result<Vec<CaptureDevice>, String> {
        Ok(Vec::new())
    }
}

/// A fixed list, for tests and the debug-only `HELM_TEST_MICS` seam.
pub struct FixedAudio(pub Vec<CaptureDevice>);
impl AudioInputs for FixedAudio {
    fn capture_devices(&self) -> Result<Vec<CaptureDevice>, String> {
        Ok(self.0.clone())
    }
}

pub struct NativeAudio;

#[cfg(windows)]
impl AudioInputs for NativeAudio {
    fn capture_devices(&self) -> Result<Vec<CaptureDevice>, String> {
        std::thread::spawn(enumerate_windows)
            .join()
            .map_err(|_| "audio enumeration panicked".to_owned())?
    }
}

#[cfg(windows)]
fn enumerate_windows() -> Result<Vec<CaptureDevice>, String> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };
    // SAFETY: COM is initialised on this dedicated thread and released before it ends; every
    // interface is dropped before CoUninitialize, and every CoTaskMem string is freed.
    unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED);
        let result = (|| -> windows::core::Result<Vec<CaptureDevice>> {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let default_id = en
                .GetDefaultAudioEndpoint(eCapture, eConsole)
                .ok()
                .and_then(|d| d.GetId().ok())
                .map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                });
            let coll = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
            let mut out = Vec::new();
            for i in 0..coll.GetCount()? {
                let dev = coll.Item(i)?;
                let pid = dev.GetId()?;
                let id = pid.to_string().unwrap_or_default();
                CoTaskMemFree(Some(pid.0 as *const _));
                let name = dev
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|s| s.GetValue(&PKEY_Device_FriendlyName))
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let name = if name.trim().is_empty() {
                    "Microphone".to_owned()
                } else {
                    name
                };
                out.push(CaptureDevice {
                    is_default: default_id.as_deref() == Some(id.as_str()),
                    id,
                    name,
                });
            }
            Ok(out)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result.map_err(|e| format!("capture devices: {e}"))
    }
}

#[cfg(not(windows))]
impl AudioInputs for NativeAudio {
    fn capture_devices(&self) -> Result<Vec<CaptureDevice>, String> {
        Ok(Vec::new())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn enumeration_runs_and_marks_at_most_one_default() {
        // Machines without a microphone return an empty list; either way it must not fail.
        let list = NativeAudio
            .capture_devices()
            .expect("Core Audio enumeration");
        assert!(list.iter().filter(|d| d.is_default).count() <= 1);
        assert!(list.iter().all(|d| !d.id.is_empty() && !d.name.is_empty()));
    }
}
