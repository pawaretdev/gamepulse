//! Windows notifications (toasts) for MVP alerts.
//!
//! An unpackaged exe can only raise toasts under an AppUserModelID that Windows knows: `register`
//! writes `HKCU\Software\Classes\AppUserModelId\Gamepulse` (display name + icon) and tags this
//! process with it, so toasts show as "Gamepulse" with the app icon.
use windows::{
    core::{HSTRING, PCWSTR},
    Data::Xml::Dom::XmlDocument,
    Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ},
        UI::Shell::SetCurrentProcessExplicitAppUserModelID,
    },
    UI::Notifications::{ToastNotification, ToastNotificationManager},
};

/// Registry key and app data folder name; what Windows shows is the DisplayName below
const APP_ID: &str = "Gamepulse";
const ICON_PNG: &[u8] = include_bytes!("../../assets/icon-256.png");

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn set_value(key: &str, name: &str, value: &str) -> Result<(), String> {
    let (key, name, value) = (wide(key), wide(name), wide(value));
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            (value.len() * 2) as u32,
        )
    };
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry error {}", result.0))
    }
}

/// Call once at startup, before the first toast
pub fn register() -> Result<(), String> {
    let key = format!(r"Software\Classes\AppUserModelId\{APP_ID}");
    set_value(&key, "DisplayName", "Gamepulse")?;
    // The icon must be a file; keep a copy in the user's local app data
    if let Some(dir) =
        std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join(APP_ID))
    {
        let icon = dir.join("toast-icon.png");
        if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&icon, ICON_PNG).is_ok() {
            set_value(&key, "IconUri", &icon.to_string_lossy())?;
        }
    }
    unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID)) }
        .map_err(|e| format!("app id: {e}"))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn show(title: &str, body: &str) -> Result<(), String> {
    super::ocr::init_thread();
    let xml = format!(
        r#"<toast><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#,
        escape(title),
        escape(body)
    );
    let document = XmlDocument::new().map_err(|e| e.to_string())?;
    document
        .LoadXml(&HSTRING::from(xml))
        .map_err(|e| format!("toast XML: {e}"))?;
    let toast = ToastNotification::CreateToastNotification(&document).map_err(|e| e.to_string())?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .and_then(|notifier| notifier.Show(&toast))
        .map_err(|e| format!("Windows notification failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_text_is_escaped() {
        assert_eq!(
            escape(r#"<b>"A&B"</b>"#),
            "&lt;b&gt;&quot;A&amp;B&quot;&lt;/b&gt;"
        );
    }
}
