# Which SKUs work with Govee2MQTT?

Support depends largely on what Govee exposes via its documented APIs.
There are some devices for which the undocumented APIs have been
reverse engineered.

If the device has no WiFi, then Govee2MQTT is not able to control
it at this time, as there is no BLE support in Govee2MQTT at this time.

Only devices that support the LAN API are able to be controlled locally without
internet access, however, the LAN API only enables a subset of the full device
functionality. All known LAN API compatible devices are lights; there are no
known appliance devices that support fully local control. This is not a
limitation of Govee2MQTT, but a limitation of the hardware itself.

## IoT API Auto-Detection

Govee2MQTT can automatically detect whether a device supports the IoT API
(AWS MQTT) based on data from the undocumented API and observed state updates.
Devices that previously required a manual quirk entry for IoT control may now
work automatically. Explicit quirk entries still take precedence.

## Device Families

|Family|LAN API?|Platform API?|IoT API?|
|------|--------|-------------|--------|
|Lights/LED Strips|The more modern/powerful WiFi controller chips can have LAN API enabled through the Govee App. When enabled, the device can have its color/temperature, brightness and on/off state controlled locally, with no external network connection required.|Most WiFi enabled controller chips can be controlled via Govee's cloud-based Platform API.|Most WiFi lights support IoT for fast control and state updates. Auto-detected.|
|Humidifiers|Not supported|Most humidifiers are controllable via the Platform API, but the level of control can be patchy.|Some models (e.g. H7160) support IoT for nightlight control.|
|Kettles|Not supported|Tested with H7171 and H7173.|No|
|Heaters, Fans, Purifiers|Not supported|Tested with H7101, H7102, H7105, H7111, H7121, H7130, H7131, H713A, H7135.|Fan entity support is not yet implemented (detected as device type but no control entities).|
|Ice Makers|Not supported|Yes (e.g. H7172).|No|
|Thermometers|Not supported|Temperature and humidity sensors (e.g. H5051, H5100, H5179).|No|
|Plugs|Not supported|Yes, but the API is buggy and support may be limited.|No|
