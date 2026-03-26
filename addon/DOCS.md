# Govee to MQTT Bridge

This Home Assistant app runs `govee2mqtt` inside Home Assistant OS or Supervised Home Assistant and exposes Govee devices to Home Assistant through MQTT discovery.

## What it needs

- A working Home Assistant MQTT integration.
- Usually the Mosquitto Broker app, or another reachable MQTT broker.
- Optional Govee credentials if you want cloud, Platform API, or undocumented IoT features.

## Configuration

Common options:

- `temperature_scale`: `C` or `F`
- `govee_email` / `govee_password`: Enables Govee account login features (IoT, one-click scenes, room names)
- `govee_api_key`: Enables official Govee Platform API features (scenes, device metadata, real-time push updates)
- `mqtt_host` / `mqtt_port` / `mqtt_username` / `mqtt_password`: Override broker auto-discovery if you are not using the Mosquitto app
- `debug_level`: Rust log filter such as `govee=trace`
- `no_multicast`, `broadcast_all`, `global_broadcast`, `scan`: LAN discovery tuning
- `disable_effects`: Disable effects in MQTT discovery (fixes Google Home offline issue)
- `allowed_effects`: Comma-separated whitelist of effect names to include

If `mqtt_host` is left empty, the app waits for the Home Assistant MQTT service and uses the broker details provided by Supervisor.

## Web UI

The app web UI is exposed through Home Assistant Ingress. Open it from the app page with **Open Web UI**.

The Web UI has three tabs:

- **Devices** — Device table with power, brightness, color, and scene controls. Click a row to expand device details (capabilities, active scene, state JSON).
- **Logs** — Live log viewer with WebSocket streaming and text filter. Shows the last 500 log entries.
- **Bridge** — Bridge health status, push API connection state, and MQTT command reference.

## Per-Device Configuration

Create a JSON file at `/data/govee-device-config.json` (inside the app container) to override per-device settings. The file is **hot-reloaded** — changes take effect automatically without restarting the app.

Example:

```json
{
  "devices": {
    "AA:BB:CC:DD:EE:FF:00:11": {
      "name": "Kitchen Light",
      "color_temp_range": [2700, 6500],
      "room": "Kitchen",
      "disable_effects": true
    },
    "H6076": {
      "icon": "mdi:floor-lamp",
      "prefer_lan": true
    }
  },
  "groups": {
    "living-room": {
      "name": "Living Room Lights",
      "members": ["AA:BB:CC:DD", "EE:FF:00:11"],
      "room": "Living Room"
    }
  }
}
```

Keys can be device IDs (exact match) or SKU model numbers (applies to all devices of that model). Device ID matches take priority over SKU matches.

Available overrides: `name`, `color_temp_range`, `prefer_lan`, `disable_effects`, `room`, `icon`.

Groups appear as a single light entity in HA that controls all member devices in parallel.

## External Device Quirks

If your device model isn't recognized, create `/data/govee-quirks.json`:

```json
[
  {
    "sku": "H9999",
    "icon": "mdi:lightbulb",
    "supports_rgb": true,
    "supports_brightness": true,
    "lan_api_capable": true,
    "device_type": "light"
  }
]
```

## MQTT Topics

The bridge publishes to several MQTT topics for monitoring and automation:

- `gv2mqtt/bridge/health` — Device counts, API connectivity, push stats
- `gv2mqtt/bridge/devices` — Full device list (retained)
- `gv2mqtt/bridge/error` — Error messages when operations fail
- `gv2mqtt/{device}/availability` — Per-device online/offline
- `gv2mqtt/{device}/push_event` — Real-time Govee push events
- `gv2mqtt/{device}/lack_water` — Humidifier low water alerts

You can also control the bridge via MQTT:

- `gv2mqtt/bridge/request/restart` — Restart the bridge
- `gv2mqtt/bridge/request/cache_purge` — Purge caches
- `gv2mqtt/bridge/request/config_reload` — Reload device config
- `gv2mqtt/bridge/request/log_level` — Change log level (payload: `trace`, `debug`, `info`, etc.)

## Troubleshooting

### Scenes don't work in automations

Subscribe to `gv2mqtt/bridge/error` for error details. Common causes: rate limiting, IoT connection dropped, or scene was renamed. See the FAQ for a full troubleshooting guide.

### Google Home shows devices offline

Set `disable_effects: true` in the app config. See the [configuration docs](https://github.com/sitapix/govee2mqtt/blob/main/docs/CONFIG.md#effect-list-filtering) for details.

### "Cannot bind to UDP Port 4002"

Another integration (Matter Server) is using port 4002. If running as a Docker container, set the `GOVEE_LAN_LISTEN_PORT` environment variable to a different port. This option is not available in the Home Assistant app — you will need to stop the conflicting app first.

### Rate limited

Enable the API key to get real-time push updates without polling. Enable LAN control to avoid API usage for supported devices.

## Notes

- `host_network: true` is required because Govee LAN discovery depends on local-network broadcast and multicast traffic.
- Device entities are created by Home Assistant's MQTT integration, not by a Python custom integration in this repository.
- The bridge gracefully handles API outages by falling back to cached device data.
- Log files are written to `/data/govee2mqtt.log` with automatic rotation (3 files x 10MB).

## More help

- Repo: https://github.com/sitapix/govee2mqtt
- Configuration: https://github.com/sitapix/govee2mqtt/blob/main/docs/CONFIG.md
- FAQ: https://github.com/sitapix/govee2mqtt/blob/main/docs/FAQ.md
