# Music Mode with Color Palettes

Govee's music mode combines a motion style with colors used by reactive
points. The Platform API can select a style and one packed RGB color, but it
cannot express a multi-color palette. This feature sends the device's
reverse-engineered `ptReal` frames over LAN.

## Enable it

Set the following environment variable and restart the bridge:

```text
GOVEE_MUSIC_PALETTE=true
```

The feature is off by default and requires LAN Control to be enabled for the
device in the Govee Home app.

## Usage

Publish JSON to `gv2mqtt/<device-id>/set-music-palette`:

```json
{"style":"Rhythm","colors":["#ff7a00","#1400c8","#4a00e0"],"sensitivity":99}
```

- `style` is case-insensitive and must be mapped for the device SKU.
- `colors` accepts one to seven `#rrggbb` values.
- `sensitivity` is optional, ranges from 0 to 100, and defaults to 100.

The UDP frame sequence is sent twice, 300 ms apart, because LAN commands have
no acknowledgement. Brightness remains controlled through the normal light
entity. Setting a static color or color temperature exits music mode on tested
devices.

## Supported devices

Profile IDs are SKU-specific and cannot be copied between models.

| SKU | Styles |
|-----|--------|
| H607C | Touching, Rhythm, Splash, Stippling, Hopping, Luminous, Blend, Fantasy, Spring |
| H6020 | Rhythm, Beat A, Gridding, Energic, Dandelion, Drifting |
| H60B0 | Stippling, Hopping, Luminous, Rhythm, Flowing Light, Sprouting, Shiny |

These mappings were captured and visually verified on hardware. Other SKUs may
use a different frame dialect and are intentionally rejected until mapped.

## Mapping another SKU

Use the Govee app to activate each style and inspect the next AWS IoT status
push. Its `op.command` includes `aa 05 13 <profile> <sensitivity>`; the profile
byte identifies that style for that SKU. Verify every mapping on reachable
hardware before contributing it, since malformed sequences can require a power
cycle on some devices.
