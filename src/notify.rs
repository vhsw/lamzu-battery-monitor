use std::sync::Once;

use anyhow::{Context, Result};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
use windows::core::{HSTRING, PCWSTR};

use crate::config::APP_NAME;

const TOAST_APP_ID: &str = "Microsoft.Windows.Explorer";

pub fn show_toast(title: &str, message: &str) -> Result<()> {
    static WINRT_INIT: Once = Once::new();

    WINRT_INIT.call_once(|| unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    });

    let xml = format!(
        r#"<toast><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#,
        xml_escape(title),
        xml_escape(message)
    );

    let doc = XmlDocument::new().context("creating XmlDocument")?;
    doc.LoadXml(&HSTRING::from(xml))
        .context("loading toast XML")?;

    let notification =
        ToastNotification::CreateToastNotification(&doc).context("creating toast notification")?;

    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(TOAST_APP_ID))
            .context("creating toast notifier")?;

    notifier
        .Show(&notification)
        .context("showing toast notification")?;

    Ok(())
}

pub fn show_fatal(message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let caption: Vec<u16> = APP_NAME.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR::from_raw(text.as_ptr()),
            PCWSTR::from_raw(caption.as_ptr()),
            MB_ICONWARNING | MB_OK,
        );
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
