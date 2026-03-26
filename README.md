# Govee to MQTT bridge for Home Assistant

This repo provides a `govee` executable whose primary purpose is to act
as a bridge between [Govee](https://govee.com) devices and Home Assistant,
via the [Home Assistant MQTT Integration](https://www.home-assistant.io/integrations/mqtt/).

## Features

* Robust LAN-first design. Not all of Govee's devices support LAN control,
  but for those that do, you'll have the lowest latency and ability to
  control them even when your primary internet connection is offline.
* Support for per-device modes and scenes, including dedicated scene selects
  for lightScene, diyScene, snapshot, nightlightScene, and music modes.
* Support for the undocumented AWS IoT interface to your devices, providing
  low latency status updates.
* Support for the official [Platform
  API](https://developer.govee.com/reference/get-you-devices) in case the AWS
  IoT or LAN control is unavailable.
* Real-time state updates via the official Govee MQTT push API (requires API key).
* Per-device and per-segment color control via LAN.
* Device grouping — control multiple devices as one light entity.
* Per-device configuration overrides via JSON file (names, color temp, icons, rooms).
* Web UI with device controls, live log viewer, and bridge status dashboard.
* Graceful shutdown with proper MQTT offline status publishing.
* Persistent device database for offline/degraded mode operation.

|Feature|Requires|Notes|
|-------|--------|-------------|
|DIY Scenes|API Key|Find in the list of Effects for the light in Home Assistant|
|Music Modes|API Key|Find in the list of Effects for the light in Home Assistant|
|Tap-to-Run / One Click Scene|IoT|Find in the overall list of Scenes in Home Assistant, as well as under the `Govee to MQTT` device|
|Live Device Status Updates|LAN and/or IoT and/or API Key|Devices typically report most changes within a couple of seconds.|
|Segment Color|API Key or LAN|Find the `Segment 00X` light entities associated with your main light device in Home Assistant|
|Energy Monitoring|API Key|Smart plugs expose power, voltage, current, and energy sensors|
|Effect List Filtering|API Key|Disable or filter effects for Google Home compatibility|
|Device Groups|Config file|Control multiple devices as a single HA light entity|
|ptReal Command Replay|LAN or IoT|Send captured DIY scene commands via HTTP API|

### API Channels

| Channel | Needs | Control | Status | Latency |
|---------|-------|---------|--------|---------|
| LAN | Device on network + LAN enabled | Full (power, color, brightness, scenes, segments) | Real-time broadcast | Lowest |
| IoT | Govee email + password | Full + one-click scenes | Real-time push | Low |
| Platform API | API key | Full except one-click | Poll (120s default) | Medium |
| Govee Push | API key | Read-only | Real-time push | Low |

The bridge automatically picks the best available channel for each device and command.

* `API Key` means that you have [applied for a key from Govee](https://developer.govee.com/reference/apply-you-govee-api-key)
  and have configured it for use in govee2mqtt
* `IoT` means that you have configured your Govee account email and password for
  use in govee2mqtt, which will then attempt to use the
  *undocumented and likely unsupported* AWS MQTT-based IoT service
* `LAN` means that you have enabled the [Govee LAN API](https://app-h5.govee.com/user-manual/wlan-guide)
  on supported devices and that the LAN API protocol is functional on your network

## Usage

* [Installing the HASS App](docs/ADDON.md) - for HAOS and Supervised HASS users
* [Running it in Docker](docs/DOCKER.md)
* [Configuration](docs/CONFIG.md)

## Development

```bash
cp .env.example .env        # fill in your Govee credentials
make dev-up                  # builds from source + starts Mosquitto + govee2mqtt
make dev-logs                # tail logs
make dev-rebuild             # rebuild after code changes
make dev-down                # stop everything
```

Web UI: `http://localhost:8056` | MQTT: `localhost:1883` | Health: `http://localhost:8056/api/health`

### Testing

```bash
make test                              # unit tests (131 tests)
cargo test --test lan_simulator        # LAN protocol simulator (4 tests)
cargo test --test mqtt_integration -- --test-threads=1  # MQTT integration (3 tests, needs Docker)
```

## MQTT Topics

### Bridge Topics

| Topic | Retained | Description |
|-------|----------|-------------|
| `gv2mqtt/availability` | Yes | Bridge online/offline (LWT) |
| `gv2mqtt/bridge/info` | Yes | Version and state |
| `gv2mqtt/bridge/health` | Yes | Device counts, API status, push stats |
| `gv2mqtt/bridge/devices` | Yes | Full device list with availability |
| `gv2mqtt/bridge/error` | No | Error messages for failed operations |

### Bridge Request/Response API

Publish to these topics to control the bridge via MQTT:

| Request Topic | Payload | Description |
|---------------|---------|-------------|
| `gv2mqtt/bridge/request/health` | (empty) | Publish health data |
| `gv2mqtt/bridge/request/devices` | (empty) | Publish device list |
| `gv2mqtt/bridge/request/cache_purge` | (empty) | Purge caches and re-register |
| `gv2mqtt/bridge/request/config_reload` | (empty) | Reload device config file |
| `gv2mqtt/bridge/request/restart` | (empty) | Restart the bridge |
| `gv2mqtt/bridge/request/log_level` | `trace`/`debug`/`info`/`warn`/`error` | Change log verbosity |

### Per-Device Topics

| Topic | Description |
|-------|-------------|
| `gv2mqtt/{device}/availability` | Per-device online/offline |
| `gv2mqtt/{device}/push_event` | Raw Govee push API events |
| `gv2mqtt/{device}/lack_water` | Humidifier low water alert |

## HTTP API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Bridge status (no auth required) |
| `/api/devices` | GET | Device list |
| `/api/device/{id}/inspect` | GET | Full device debug data |
| `/api/device/{id}/power/on` | POST | Turn on |
| `/api/device/{id}/power/off` | POST | Turn off |
| `/api/device/{id}/brightness/{level}` | POST | Set brightness (0-100) |
| `/api/device/{id}/color/{css_color}` | POST | Set color |
| `/api/device/{id}/colortemp/{kelvin}` | POST | Set color temperature |
| `/api/device/{id}/scene/{name}` | POST | Activate scene |
| `/api/device/{id}/scenes` | GET | List available scenes |
| `/api/device/{id}/ptreal` | POST | Send raw ptReal commands |
| `/api/config` | GET/PUT | Read or update device config |
| `/api/oneclicks` | GET | List one-click scenes |
| `/api/oneclick/activate/{scene}` | POST | Activate a one-click scene |
| `/api/logs` | GET | Recent log entries (JSON) |
| `/api/ws/logs` | WebSocket | Live log streaming |

Set `GOVEE_HTTP_AUTH_TOKEN` to require a Bearer token for API access (except `/api/health`).

## Have a question?

* [Is my device supported?](docs/SKUS.md)
* [Check out the FAQ](docs/FAQ.md)

## Want to show your support or gratitude?

It takes significant effort to build, maintain and support users of software
like this. If you can spare something to say thanks, it is appreciated!

* [Sponsor me on Github](https://github.com/sponsors/wez)
* [Sponsor me on Patreon](https://patreon.com/WezFurlong)
* [Sponsor me on Ko-Fi](https://ko-fi.com/wezfurlong)
* [Sponsor me via liberapay](https://liberapay.com/wez)

## Credits

This work is based on my earlier work with [Govee LAN
Control](https://github.com/wez/govee-lan-hass/).

The AWS IoT support was made possible by the work of @bwp91 in
[homebridge-govee](https://github.com/bwp91/homebridge-govee/).

The official Govee MQTT push API was discovered via
[govee-java-api](https://github.com/bigboxer23/govee-java-api).

LAN segment color control was contributed by
[alexluckett](https://github.com/alexluckett/govee2mqtt-segment-control).
