#![windows_subsystem = "windows"]

#[cfg(not(windows))]
compile_error!("This application only supports Windows.");

mod config;
mod device;
mod notify;
mod shutdown;

use std::path::Path;
use std::sync::mpsc::Receiver;
use std::thread;

use anyhow::{Context, Result, anyhow};
use log::{error, info};
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::AttachConsole;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostQuitMessage, PostThreadMessageW, TranslateMessage,
};

use config::{APP_NAME, Config, app_data_dir, init_logging, load_config};
use shutdown::Shutdown;

const QUIT_MENU_ID: &str = "quit";
const BATTERY_MENU_ID: &str = "battery";

const WM_BATTERY_UPDATE: u32 = 0x8001;
const WM_SHUTDOWN: u32 = 0x8002;
const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;

fn main() {
    let app_dir = app_data_dir();
    let log_dir = app_dir.join("logs");

    match init_logging(&log_dir) {
        Ok(_logger) => {
            if let Err(e) = run(&app_dir) {
                error!("Fatal error: {e:#}");
                notify::show_fatal(&format!("{APP_NAME} failed to start: {e:#}"));
            }
        }
        Err(e) => {
            notify::show_fatal(&format!("{APP_NAME} could not initialize logging: {e:#}"));
        }
    }
}

fn run(app_dir: &Path) -> Result<()> {
    info!("{APP_NAME} starting");

    device::assert_battery_command_checksum();

    let cfg = load_config(app_dir);

    info!(
        "Configuration: poll every {}s, low threshold {}%, critical threshold {}%",
        cfg.poll_interval.as_secs(),
        cfg.low_pct,
        cfg.critical_pct
    );

    let shutdown = Shutdown::new();

    // Allow this GUI process to receive Ctrl+C when started from a terminal/cargo run.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }

    let ctrl_shutdown = shutdown.clone();
    let ui_thread_id = unsafe { GetCurrentThreadId() };

    ctrlc::set_handler(move || {
        info!("Ctrl+C received, shutting down");
        ctrl_shutdown.stop();

        unsafe {
            let _ = PostThreadMessageW(ui_thread_id, WM_SHUTDOWN, WPARAM(0), LPARAM(0));
        }
    })
    .map_err(|e| anyhow!("failed to install Ctrl+C handler: {e}"))?;

    let bg_shutdown = shutdown.clone();
    let bg_cfg = cfg.clone();

    let (battery_tx, battery_rx) = std::sync::mpsc::channel::<u8>();

    let bg_thread = thread::Builder::new()
        .name("battery-poll".into())
        .spawn(move || device::poll_loop(bg_cfg, bg_shutdown, battery_tx))
        .context("spawning background HID poll thread")?;

    let ui_result = run_ui(&shutdown, &cfg, battery_rx);

    match ui_result {
        Ok(()) => info!("Exiting cleanly"),
        Err(ref e) => error!("UI loop ended with error: {e:#}"),
    }

    shutdown.stop();

    if bg_thread.join().is_err() {
        error!("Background poll thread panicked");
    }

    ui_result
}

fn run_ui(shutdown: &Shutdown, cfg: &Config, battery_rx: Receiver<u8>) -> Result<()> {
    let (mut tray, battery_item) = create_tray_icon(cfg).context("creating tray icon")?;

    info!("Tray icon is active");

    // The tray icon must be updated from the thread that owns the tray/UI message loop.
    //
    // The HID poll thread sends battery percentages through a channel. This small bridge
    // thread converts them into a Windows thread message so the main GetMessageW loop
    // wakes up and updates the tray icon/menu/tooltip.
    let ui_thread_id = unsafe { GetCurrentThreadId() };

    thread::Builder::new()
        .name("battery-ui-notify".into())
        .spawn(move || {
            while let Ok(pct) = battery_rx.recv() {
                unsafe {
                    let _ = PostThreadMessageW(
                        ui_thread_id,
                        WM_BATTERY_UPDATE,
                        WPARAM(pct as usize),
                        LPARAM(0),
                    );
                }
            }
        })
        .context("spawning battery UI notifier thread")?;

    run_message_loop(shutdown, &mut tray, &battery_item, cfg)
}

fn create_tray_icon(cfg: &Config) -> Result<(TrayIcon, MenuItem)> {
    let battery_item = MenuItem::with_id(BATTERY_MENU_ID, "Battery: --", false, None);
    let quit_item = MenuItem::with_id(QUIT_MENU_ID, "Exit", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &battery_item as &dyn IsMenuItem,
        &quit_item as &dyn IsMenuItem,
    ])
    .map_err(|e| anyhow!("failed to append tray menu items: {e}"))?;

    let icon = build_tray_icon(None, cfg);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip(&format!("{APP_NAME} — Battery: --"))
        .build()
        .map_err(|e| anyhow!("failed to build tray icon: {e}"))?;

    Ok((tray, battery_item))
}
fn run_message_loop(
    shutdown: &Shutdown,
    tray: &mut TrayIcon,
    battery_item: &MenuItem,
    cfg: &Config,
) -> Result<()> {
    if shutdown.is_stopped() {
        return Ok(());
    }

    unsafe {
        let mut msg = MSG::default();

        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            if msg.message == WM_SHUTDOWN {
                info!("Shutdown message received, quitting UI loop");
                shutdown.stop();
                PostQuitMessage(0);
                continue;
            }

            if msg.message == WM_BATTERY_UPDATE {
                let pct = msg.wParam.0 as u8;
                update_battery_ui(pct, tray, battery_item, cfg);
            } else {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            while TrayIconEvent::receiver().try_recv().is_ok() {}

            while let Ok(evt) = MenuEvent::receiver().try_recv() {
                if evt.id().0.as_str() == QUIT_MENU_ID {
                    info!("Exit requested from tray menu");
                    shutdown.stop();
                    PostQuitMessage(0);
                }
            }
        }
    }

    Ok(())
}

fn update_battery_ui(pct: u8, tray: &mut TrayIcon, battery_item: &MenuItem, cfg: &Config) {
    let pct = pct.min(100);

    let _ = tray.set_icon(Some(build_tray_icon(Some(pct), cfg)));
    let _ = tray.set_tooltip(Some(format!("{APP_NAME} — Battery: {pct}%")));
    battery_item.set_text(&format!("Battery: {pct}%"));
}

fn build_tray_icon(pct: Option<u8>, cfg: &Config) -> Icon {
    const SIZE: u32 = 32;

    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    let outline: [u8; 4] = [255, 255, 255, 255];

    let fill: [u8; 4] = match pct {
        Some(p) if p <= cfg.critical_pct => [255, 69, 69, 255],
        Some(p) if p <= cfg.low_pct => [255, 179, 0, 255],
        Some(_) => [255, 255, 255, 255],
        None => [0, 0, 0, 0],
    };

    // Inner fill area in the original icon is x = 4..26, width = 22 px.
    let fill_width: u32 = match pct {
        Some(p) => {
            let p = p.min(100) as u32;
            let width = (22 * p + 50) / 100;

            // Show at least one pixel for 1%..4%, otherwise rounding can hide it.
            if p > 0 && width == 0 { 1 } else { width }
        }
        None => 0,
    };

    for y in 0..SIZE {
        for x in 0..SIZE {
            let idx = ((y * SIZE + x) * 4) as usize;

            let battery_body_outline = (x >= 1 && x < 29 && (y == 7 || y == 24))
                || (y >= 7 && y < 25 && (x == 0 || x == 28));

            let battery_tip = x >= 29 && x < 32 && y >= 13 && y < 19;

            let battery_fill = fill_width > 0 && x >= 4 && x < 4 + fill_width && y >= 10 && y < 22;

            let color = if battery_body_outline || battery_tip {
                outline
            } else if battery_fill {
                fill
            } else {
                continue;
            };

            rgba[idx] = color[0];
            rgba[idx + 1] = color[1];
            rgba[idx + 2] = color[2];
            rgba[idx + 3] = color[3];
        }
    }

    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid RGBA tray icon")
}
