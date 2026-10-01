use std::ffi::CString;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use hidapi::{HidApi, HidDevice};
use log::{debug, error, info, warn};

use crate::config::Config;
use crate::notify;
use crate::shutdown::Shutdown;

const VID: u16 = 0x25A7;
const PID: u16 = 0xFA7C;

const WRITE_USAGE_PAGE: u16 = 0xFF02; // Col07
const WRITE_USAGE: u16 = 0x0002;

const READ_USAGE_PAGE: u16 = 0xFF01; // Col05
const READ_USAGE: u16 = 0x0000;

const REPORT_LEN: usize = 17;
const RESPONSE_LEN: usize = 17;

const BATTERY_BYTE_INDEX: usize = 6;
const RESPONSE_REPORT_ID: u8 = 0x09;
const RESPONSE_CMD_ECHO: u8 = 0x04;

const DISCONNECTED_BACKOFF: Duration = Duration::from_secs(30);
const QUERY_WINDOW: Duration = Duration::from_millis(200);
const READ_TIMEOUT_MS: i32 = 50;
const RETRY_DELAY: Duration = Duration::from_millis(500);
const MAX_QUERY_ATTEMPTS: u32 = 3;

const BATTERY_CMD: [u8; REPORT_LEN] = [
    0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x49,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertState {
    Normal,
    Low,
    Critical,
}

pub fn assert_battery_command_checksum() {
    assert_eq!(
        checksum(&BATTERY_CMD[..16]),
        BATTERY_CMD[16],
        "BATTERY_CMD checksum constant is incorrect"
    );
}

pub fn poll_loop(cfg: Config, shutdown: Shutdown, battery_tx: Sender<u8>) {
    info!("Background poll thread started");

    let mut api: Option<HidApi> = None;
    let mut read_buf = vec![0u8; RESPONSE_LEN];
    let mut alert_state = AlertState::Normal;
    let mut connected = false;

    while !shutdown.is_stopped() {
        if api.is_none() {
            match HidApi::new() {
                Ok(new_api) => {
                    api = Some(new_api);
                    debug!("HidApi initialized");
                }
                Err(e) => {
                    warn!(
                        "HidApi initialization failed ({}); retrying in {}s",
                        e,
                        DISCONNECTED_BACKOFF.as_secs()
                    );

                    if shutdown.wait_timeout(DISCONNECTED_BACKOFF) {
                        break;
                    }

                    continue;
                }
            }
        }

        let Some(api_ref) = api.as_ref() else {
            continue;
        };

        match open_device_pair(api_ref) {
            Ok((write_handle, read_handle)) => {
                if !connected {
                    info!("Device connected (VID 0x{:04X}, PID 0x{:04X})", VID, PID);
                    connected = true;
                }

                let mut battery: Option<u8> = None;
                let mut device_error: Option<anyhow::Error> = None;
                let mut no_response = false;

                for attempt in 1..=MAX_QUERY_ATTEMPTS {
                    match query_battery(&write_handle, &read_handle, &mut read_buf) {
                        Ok(Some(pct)) => {
                            battery = Some(pct);
                            break;
                        }
                        Ok(None) => {
                            no_response = true;
                            debug!(
                                "Battery query attempt {} of {}: no valid response",
                                attempt, MAX_QUERY_ATTEMPTS
                            );
                        }
                        Err(e) => {
                            device_error = Some(e);
                            break;
                        }
                    }

                    if attempt < MAX_QUERY_ATTEMPTS && shutdown.wait_timeout(RETRY_DELAY) {
                        break;
                    }
                }

                if shutdown.is_stopped() {
                    break;
                }

                if let Some(err) = device_error {
                    connected = false;
                    warn!(
                        "HID communication failed ({err:#}); retrying in {}s",
                        DISCONNECTED_BACKOFF.as_secs()
                    );

                    if shutdown.wait_timeout(DISCONNECTED_BACKOFF) {
                        break;
                    }

                    continue;
                }

                if let Some(pct) = battery {
                    info!("Battery: {pct}%");

                    let _ = battery_tx.send(pct);

                    alert_state = update_alert_state(pct, &cfg, alert_state);

                    if shutdown.wait_timeout(cfg.poll_interval) {
                        break;
                    }
                } else if no_response {
                    debug!(
                        "No battery response (mouse asleep/off); next poll in {}s",
                        cfg.poll_interval.as_secs()
                    );

                    if shutdown.wait_timeout(cfg.poll_interval) {
                        break;
                    }
                }
            }
            Err(e) => {
                if connected {
                    info!("Device disconnected or inaccessible ({e:#})");
                    connected = false;
                } else {
                    debug!("Device not ready ({e:#})");
                }

                if shutdown.wait_timeout(DISCONNECTED_BACKOFF) {
                    break;
                }
            }
        }
    }

    info!("Background poll thread stopped");
}

fn open_device_pair(api: &HidApi) -> Result<(HidDevice, HidDevice)> {
    let mut write_path: Option<CString> = None;
    let mut read_path: Option<CString> = None;

    for dev in api.device_list() {
        if dev.vendor_id() != VID || dev.product_id() != PID {
            continue;
        }

        let iface = dev.interface_number();
        if iface != 1 && iface != -1 {
            continue;
        }

        if dev.usage_page() == WRITE_USAGE_PAGE && dev.usage() == WRITE_USAGE {
            write_path = Some(dev.path().to_owned());
        }

        if dev.usage_page() == READ_USAGE_PAGE && dev.usage() == READ_USAGE {
            read_path = Some(dev.path().to_owned());
        }
    }

    if write_path.is_none() || read_path.is_none() {
        debug!("Strict HID collection match failed; trying usage-page-only fallback");

        for dev in api.device_list() {
            if dev.vendor_id() != VID || dev.product_id() != PID {
                continue;
            }

            if write_path.is_none() && dev.usage_page() == WRITE_USAGE_PAGE {
                write_path = Some(dev.path().to_owned());
            }

            if read_path.is_none() && dev.usage_page() == READ_USAGE_PAGE {
                read_path = Some(dev.path().to_owned());
            }
        }
    }

    let write_path = write_path.context("write HID collection (0xFF02) not found")?;
    let read_path = read_path.context("read HID collection (0xFF01) not found")?;

    let write_handle = api
        .open_path(&write_path)
        .context("opening write HID collection")?;

    let read_handle = api
        .open_path(&read_path)
        .context("opening read HID collection")?;

    Ok((write_handle, read_handle))
}

fn drain_input(read: &HidDevice, buf: &mut [u8]) {
    for _ in 0..8 {
        match read.read_timeout(buf, 0) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

fn query_battery(
    write_handle: &HidDevice,
    read_handle: &HidDevice,
    buf: &mut [u8],
) -> Result<Option<u8>> {
    drain_input(read_handle, buf);

    write_handle
        .send_feature_report(&BATTERY_CMD)
        .context("send_feature_report")?;

    let deadline = Instant::now() + QUERY_WINDOW;

    while Instant::now() < deadline {
        match read_handle.read_timeout(buf, READ_TIMEOUT_MS) {
            Ok(0) => continue,
            Ok(n) if n >= RESPONSE_LEN => {
                let data = &buf[..RESPONSE_LEN];

                if data[0] == RESPONSE_REPORT_ID && data[1] == RESPONSE_CMD_ECHO {
                    if data[5] != 0x02 {
                        debug!("Unexpected response status byte 0x{:02X}", data[5]);
                    }

                    let expected = checksum(&data[..16]);
                    if expected != data[16] {
                        warn!(
                            "Response checksum mismatch: expected 0x{:02X}, got 0x{:02X}",
                            expected, data[16]
                        );
                        return Ok(None);
                    }

                    let pct = data[BATTERY_BYTE_INDEX];

                    return if pct <= 100 {
                        Ok(Some(pct))
                    } else {
                        warn!("Invalid battery percentage value: {pct}");
                        Ok(None)
                    };
                }
            }
            Ok(_) => continue,
            Err(e) => return Err(anyhow!("HID read failed: {e}")),
        }
    }

    Ok(None)
}

fn checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    0x55u8.wrapping_sub(sum)
}

fn update_alert_state(pct: u8, cfg: &Config, current: AlertState) -> AlertState {
    let new_state = if pct <= cfg.critical_pct {
        AlertState::Critical
    } else if pct <= cfg.low_pct {
        AlertState::Low
    } else {
        AlertState::Normal
    };

    match (current, new_state) {
        (AlertState::Normal, AlertState::Low) => {
            notify_low(pct);
            AlertState::Low
        }
        (AlertState::Normal, AlertState::Critical) | (AlertState::Low, AlertState::Critical) => {
            notify_critical(pct);
            AlertState::Critical
        }
        (AlertState::Critical, AlertState::Low) => AlertState::Low,
        (_, AlertState::Normal) => AlertState::Normal,
        _ => current,
    }
}

fn notify_low(pct: u8) {
    info!("Battery dropped to {pct}%, triggering low-battery notification");

    if let Err(e) = notify::show_toast(
        "Lamzu Mouse — Low Battery",
        &format!("Battery at {pct}%. Consider charging soon."),
    ) {
        error!("Toast notification failed: {e:#}");
    }
}

fn notify_critical(pct: u8) {
    info!("Battery dropped to {pct}%, triggering critical-battery notification");

    if let Err(e) = notify::show_toast(
        "Lamzu Mouse — CRITICAL",
        &format!("Battery at {pct}%. Charge immediately!"),
    ) {
        error!("Toast notification failed: {e:#}");
    }
}
