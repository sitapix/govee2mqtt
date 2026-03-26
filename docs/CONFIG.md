# Configuration Options

## Govee Credentials

While `govee2mqtt` can run without any govee credentials, it can only discover
and control the devices for which you have already enabled LAN control.

It is recommended that you configure at least your Govee username and password
prior to your first run, as that is the only way for `govee2mqtt` to determine
room names to pre-assign your lights into the appropriate Home Assistant areas.

For scene control, for devices that don't support the LAN API, a Govee API Key
is required.  If you don't already have one, [you can find instructions on
obtaining one
here](https://developer.govee.com/reference/apply-you-govee-api-key).

The API key also enables the official Govee MQTT push API for real-time status
updates without polling.

|CLI|ENV|App|Purpose|
|---|---|-----|-------|
|`--govee-email`|`GOVEE_EMAIL`|`govee_email`|The email address you registered with your govee account|
|`--govee-password`|`GOVEE_PASSWORD`|`govee_password`|The password you registered for your govee account|
|`--api-key`|`GOVEE_API_KEY`|`govee_api_key`|The API key you requested from Govee support|

*Concerned about sharing your credentials? See [Privacy](PRIVACY.md) for
information about how data is used and retained by `govee2mqtt`*

## LAN API Control

A number of Govee's devices support a local control protocol that doesn't require
your primary internet connection to be online.  This offers the lowest latency
for control and is the preferred way for `govee2mqtt` to interact with your
devices.

The [Govee LAN API is described in more detail
here](https://app-h5.govee.com/user-manual/wlan-guide), including a list of
supported devices.

*Note that you must use the Govee Home app to enable the LAN API for each
individual device before it will be possible for `govee2mqtt` to control
it via the LAN API.*

In theory the LAN API is zero-configuration and auto-discovery, but this
relies on your network supporting multicast-UDP, which is challenging
on some networks, especially across wifi access points and routers.

|CLI|ENV|App|Purpose|
|---|---|-----|-------|
|`--no-multicast`|`GOVEE_LAN_NO_MULTICAST=true`|`no_multicast`|Do not multicast discovery packets to the Govee multicast group `239.255.255.250`. It is not recommended to use this option.|
|`--broadcast-all`|`GOVEE_LAN_BROADCAST_ALL=true`|`broadcast_all`|Enumerate all non-loopback network interfaces and send discovery packets to the broadcast address of each one, individually. This may be a good option if multicast-UDP doesn't work well on your network|
|`--global-broadcast`|`GOVEE_LAN_BROADCAST_GLOBAL=true`|`global_broadcast`|Send discovery packets to the global broadcast address `255.255.255.255`. This may be a possible solution if multicast-UDP doesn't work well on your network.|
|`--scan`|`GOVEE_LAN_SCAN=10.0.0.1,10.0.0.2`|`scan`|Specify a list of addresses that should be scanned by sending them discovery packets.|
|N/A|`GOVEE_LAN_LISTEN_PORT=4002`|N/A|Override the LAN response listen port (default 4002). Useful when the Matter Server or another integration conflicts.|

[Read more about LAN API Requirements here](LAN.md)

## MQTT Configuration

In order to make your devices appear in Home Assistant, you will need to have configured Home Assistant with an MQTT broker.

  * [follow these steps](https://www.home-assistant.io/integrations/mqtt/#configuration)

You will also need to configure `govee2mqtt` to use the same broker:

|CLI|ENV|App|Purpose|
|---|---|-----|-------|
|`--mqtt-host`|`GOVEE_MQTT_HOST`|`mqtt_host`|The host name or IP address of your mqtt broker. This should be the same broker that you have configured in Home Assistant.|
|`--mqtt-port`|`GOVEE_MQTT_PORT`|`mqtt_port`|The port number of the mqtt broker. The default is `1883`|
|`--mqtt-username`|`GOVEE_MQTT_USER`|`mqtt_username`|If your broker requires authentication, the username to use|
|`--mqtt-password`|`GOVEE_MQTT_PASSWORD`|`mqtt_password`|If your broker requires authentication, the password to use|

## Effect List Filtering

If Google Home shows your Govee lights as offline, it's likely because the effect
list exceeds Google's SYNC payload size limit. Use these options to reduce or
disable the published effect list:

|ENV|App|Purpose|
|---|-----|-------|
|`GOVEE_DISABLE_EFFECTS=true`|`disable_effects`|Disable all effects in MQTT discovery. Scene control via automations still works.|
|`GOVEE_ALLOWED_EFFECTS=Forest,Aurora`|`allowed_effects`|Comma-separated whitelist of effects to include (case-insensitive).|

Per-device effect disabling is also available via the [device config file](#per-device-configuration).

## HTTP API Security

|ENV|Purpose|
|---|-------|
|`GOVEE_HTTP_AUTH_TOKEN`|When set, require this token as a Bearer header or `?token=` query param for all API requests. `/api/health` is always accessible without auth.|
|`GOVEE_HTTP_INGRESS_ONLY=true`|Restrict API access to the HA ingress proxy IP only (app use).|

## Per-Device Configuration

Create a JSON file at `govee-device-config.json` in the cache directory (controlled by
`XDG_CACHE_HOME`, or `/data` in the app) to override per-device settings.

The file is **hot-reloaded** — changes are picked up automatically without restart.

```json
{
  "devices": {
    "AA:BB:CC:DD:EE:FF:00:11": {
      "name": "Kitchen Light",
      "color_temp_range": [2700, 6500],
      "room": "Kitchen",
      "disable_effects": true,
      "icon": "mdi:ceiling-light"
    },
    "H6076": {
      "prefer_lan": true
    }
  },
  "groups": {
    "all-strips": {
      "name": "All LED Strips",
      "members": ["AA:BB:CC:DD", "EE:FF:00:11"],
      "room": "Living Room"
    }
  }
}
```

### Device Overrides

Keys can be device IDs (exact match) or SKU model numbers (all devices of that model).

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Override device name in HA |
| `color_temp_range` | [min, max] | Override color temperature range in Kelvin |
| `prefer_lan` | bool | Force LAN API when available |
| `disable_effects` | bool | Disable effects for this device |
| `room` | string | Override suggested area in HA |
| `icon` | string | MDI icon override (e.g. `mdi:floor-lamp`) |

### Device Groups

Groups appear as a single light entity in HA. Commands are sent to all members in parallel.

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Group name shown in HA |
| `members` | [string] | Device IDs to include |
| `room` | string | Suggested area |
| `icon` | string | MDI icon |

## External Device Quirks

Create a JSON file at `govee-quirks.json` in the cache directory to add or override
device quirks without code changes:

```json
[
  {
    "sku": "H9999",
    "icon": "mdi:lightbulb",
    "supports_rgb": true,
    "supports_brightness": true,
    "color_temp_range": [2700, 6500],
    "lan_api_capable": true,
    "iot_api_supported": true,
    "device_type": "light"
  }
]
```

## Advanced

|ENV|App|Purpose|
|---|-----|-------|
|`RUST_LOG=govee=trace`|`debug_level`|Set log verbosity|
|`GOVEE_LOG_SENSITIVE_DATA=true`|N/A|Include API tokens in logs (debugging only)|
|`GOVEE_CACHE_DIR=/path`|N/A|Override cache directory|
|`GOVEE_TEMPERATURE_SCALE=F`|`temperature_scale`|Use Fahrenheit (default: Celsius)|
|`GOVEE_POLL_INTERVAL=120`|`poll_interval`|Platform API polling interval in seconds (default: 120). Increase to 900 if you have many devices without IoT/LAN support to stay under the 10,000 req/day API limit.|
