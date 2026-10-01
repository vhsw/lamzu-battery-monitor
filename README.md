# Lamzu Battery Monitor

Windows tray app that polls the battery level of a Lamzu Atlantis Mini wireless mouse via its 2.4G USB receiver and sends a toast notification when it gets low.

## Usage

1. Grab `lamzu-battery-monitor.exe` from [Releases](../../releases).
2. Run it. Tray icon appears bottom-right.
3. To auto-start: drop it in `shell:startup` (`Win+R` → `shell:startup`).

## Config (optional)

Place `config.toml` next to the exe or in `%LOCALAPPDATA%\LamzuBatteryMonitor\`:

```toml
# Poll battery every 5 minutes.
poll_interval_secs = 300

# Alert when battery falls to or below 20%.
low_battery_pct = 20

# Critical alert when battery falls to or below 10%.
critical_battery_pct = 10

```

## Disclaimer

This project is 100% vibecoded.
No affiliation with Lamzu.
Use at your own risk.
