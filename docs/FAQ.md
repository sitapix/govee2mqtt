# Frequently asked Questions

## Why can't I turn off a Segment?

For devices with LAN support, segments can be turned off by setting the color
to black (0,0,0) via the LAN ptReal protocol. This happens automatically when
you turn off a segment entity in Home Assistant if the device has LAN enabled.

For devices without LAN support, the Platform API can only set brightness and
color for segments, not power state. The segment entity will show a power toggle
in Home Assistant but it may not work for all devices.

## Why is my control over a Segment limited?

Govee to MQTT merely passes your control requests on to the Govee device,
and what happens next depends upon Govee. Some devices are more flexible
than others.  For example, some devices cannot set a segment brightness to 0,
while others have their individual brightness bound to the brightness of
the overall light entity.

If your device supports segments in the Govee app but they don't appear in HA,
it may be because the Platform API doesn't report segment capabilities. You can
add a `segment_count` to the device quirk via the external quirks file. See
[Configuration](CONFIG.md) for details.

## My segments don't show up but they work in the Govee app

Some devices (e.g., H7050, H7051) support segments in the app but the Platform
API doesn't report the `segmentedColorRgb` capability. These are handled via
device quirks. If your device is missing, you can add it via the
`govee-quirks.json` file — see [Configuration](CONFIG.md).

## How do I enable Video Effects for a Light?

The Govee API doesn't support returning video effects, so they are not made
available in the list of effects for a light.

What you can do to make video effects available in Home Assistant is to use the
Govee Home App to create either a "Tap-to-Run" shortcut or a saved "Snapshot"
that activates the desired mode for the device.

Then, go to the "Govee to MQTT" device in the MQTT integration in Home
Assistant and click the "Purge Caches" button.

* Tap-to-Run will be mapped into Home Assistant as a Scene entity.
* Snapshots will appear in the list of Effects on the device itself.

You can also send raw captured scene commands via the
`POST /api/device/{id}/ptreal` endpoint — see the README for details.

## My Tap-to-Run / One-Click scenes don't work in automations

If scenes work when you click them in the HA UI but fail silently in
automations, check the govee2mqtt logs. Common causes:

1. **Rate limiting** — the undocumented API that fetches one-click data
   has rate limits. Errors are published to `gv2mqtt/bridge/error`.
2. **IoT connection dropped** — one-click scenes require the AWS IoT
   connection (email + password). Check the bridge health topic.
3. **Scene was renamed** — purge caches after renaming scenes in the Govee app.

Subscribe to `gv2mqtt/bridge/error` in HA to get notified when scene
activation fails:

```yaml
automation:
  trigger:
    platform: mqtt
    topic: gv2mqtt/bridge/error
  action:
    service: notify.mobile_app
    data:
      message: "{{ trigger.payload }}"
```

## I'm hitting the Govee API rate limit (10,000 req/day)

The Platform API has a 10,000 request per day limit. To reduce usage:

1. **Enable the Govee push API** — set your API key. The push API provides
   real-time state updates without polling.
2. **Enable LAN control** — LAN devices don't use the API for status updates.
3. **Enable IoT** — set email + password. IoT provides real-time push updates.
4. The default poll interval is 120 seconds. With push and LAN handling most
   updates, the API is only used as a fallback.

The bridge logs a warning when rate limit remaining drops below 1,000.

## Google Home shows my lights as offline

This is caused by the effect list exceeding Google's SYNC payload size limit.
Set `disable_effects: true` in the app config, or use `allowed_effects`
to whitelist only the effects you need. See [Configuration](CONFIG.md).

## My Device(s) appear as Greyed Out and Unavailable in Home Assistant

This suggests that there is a problem with (re)registering the entity
in Home Assistant.

There may be more information available in the Home Assistant logs.  Look for
log entries that reference `gv2mqtt` or `mqtt`.

Check the bridge health topic (`gv2mqtt/bridge/health`) or visit the Web UI's
Bridge tab for API connectivity status.

You may also wish to try deleting the device(s) from the MQTT integration
in Home Assistant, then going to the "Govee to MQTT" device and clicking
the "Purge Caches" button.

## My device flashes briefly every minute when turned off

This was caused by LAN status polling waking the device's LEDs. This is now
fixed — devices in the OFF state are not polled via LAN.

## "database disk image is malformed"

The SQLite cache file has become corrupted. This is now handled automatically —
the corrupt file is deleted and rebuilt on startup. If the problem persists,
delete the cache file manually: `govee2mqtt-cache.sqlite` in the cache directory.

## "Cannot bind to UDP Port 4002"

Another integration (Matter Server, Govee LAN Control, homebridge-govee) is
already using port 4002. Set `GOVEE_LAN_LISTEN_PORT` to a different port number.

## DNS errors on startup

If you see "failed to lookup address information: Name does not resolve", check:

1. Your Docker container's DNS configuration
2. Network connectivity to `openapi.api.govee.com`
3. If the problem is transient, the bridge will use the persistent device
   database to continue in degraded LAN-only mode.

## Is my device supported?

Check out [this page](SKUS.md) for more details on supported devices.

You can also add custom device quirks via the `govee-quirks.json` file without
waiting for a code update — see [Configuration](CONFIG.md).

## The device MAC addresses shown in the logs don't match the MACs on my network!?

Govee device IDs are not network MAC addresses. For some devices the device ID
is a superset of the BLE MAC for the device, but if you look carefully you'll
see that the device ID is too large to be a MAC.

## This device should be available via the LAN API, but didn't respond to probing yet

Look at [this page](LAN.md) for more details on the LAN API and things you can try.

## "devices not belong you" error in logs

This error appears to be returned from Govee when trying to use the Platform
API with devices that are BLE-only and have no WiFi support.  Please file an
issue about this so that we can add an entry to the quirks database, or add
it yourself via the `govee-quirks.json` file.
