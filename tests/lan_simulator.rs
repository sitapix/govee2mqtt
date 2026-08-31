//! LAN protocol simulator for testing govee2mqtt without real hardware.
//!
//! Simulates a Govee device on localhost that responds to LAN API
//! scan, status, and control requests.
//!
//! Run with: cargo test --test lan_simulator

use govee::lan_api::{DeviceColor, DeviceStatus};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;

/// A simulated Govee device that responds to LAN protocol messages.
#[allow(dead_code)]
struct SimulatedDevice {
    sku: String,
    device_id: String,
    state: Arc<Mutex<DeviceStatus>>,
    /// Port on which the simulated device listens for commands
    cmd_port: u16,
    /// Port on which it listens for scan requests
    scan_port: u16,
}

impl SimulatedDevice {
    async fn start(sku: &str, device_id: &str) -> anyhow::Result<Self> {
        let state = Arc::new(Mutex::new(DeviceStatus {
            on: true,
            brightness: 100,
            color: DeviceColor {
                r: 255,
                g: 128,
                b: 0,
            },
            color_temperature_kelvin: 4000,
        }));

        // Bind to random ports on localhost
        let scan_socket = UdpSocket::bind("127.0.0.1:0").await?;
        let cmd_socket = UdpSocket::bind("127.0.0.1:0").await?;
        let scan_port = scan_socket.local_addr()?.port();
        let cmd_port = cmd_socket.local_addr()?.port();

        let sku_clone = sku.to_string();
        let device_id_clone = device_id.to_string();
        let state_clone = state.clone();

        // Spawn scan responder
        let sku_for_scan = sku_clone.clone();
        let id_for_scan = device_id_clone.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let Ok((len, src)) = scan_socket.recv_from(&mut buf).await else {
                    break;
                };
                let Ok(text) = std::str::from_utf8(&buf[..len]) else {
                    continue;
                };

                if text.contains("\"scan\"") {
                    let response = json!({
                        "msg": {
                            "cmd": "scan",
                            "data": {
                                "ip": "127.0.0.1",
                                "device": id_for_scan,
                                "sku": sku_for_scan,
                                "bleVersionHard": "1.0.0",
                                "bleVersionSoft": "1.0.0",
                                "wifiVersionHard": "1.0.0",
                                "wifiVersionSoft": "1.0.0",
                            }
                        }
                    });
                    let _ = scan_socket
                        .send_to(response.to_string().as_bytes(), src)
                        .await;
                }
            }
        });

        // Spawn command responder
        let state_for_cmd = state_clone.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let Ok((len, src)) = cmd_socket.recv_from(&mut buf).await else {
                    break;
                };
                let Ok(text) = std::str::from_utf8(&buf[..len]) else {
                    continue;
                };

                if text.contains("\"devStatus\"") {
                    let current = state_for_cmd.lock().unwrap().clone();
                    let response = json!({
                        "msg": {
                            "cmd": "devStatus",
                            "data": {
                                "onOff": if current.on { 1 } else { 0 },
                                "brightness": current.brightness,
                                "color": {
                                    "r": current.color.r,
                                    "g": current.color.g,
                                    "b": current.color.b,
                                },
                                "colorTemInKelvin": current.color_temperature_kelvin,
                            }
                        }
                    });
                    let _ = cmd_socket
                        .send_to(response.to_string().as_bytes(), src)
                        .await;
                } else if text.contains("\"turn\"") {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                        if let Some(val) =
                            parsed.pointer("/msg/data/value").and_then(|v| v.as_u64())
                        {
                            state_for_cmd.lock().unwrap().on = val != 0;
                        }
                    }
                } else if text.contains("\"brightness\"") {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                        if let Some(val) =
                            parsed.pointer("/msg/data/value").and_then(|v| v.as_u64())
                        {
                            state_for_cmd.lock().unwrap().brightness = val as u8;
                        }
                    }
                } else if text.contains("\"colorwc\"") {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                        let data = &parsed["msg"]["data"];
                        if let (Some(r), Some(g), Some(b)) = (
                            data["color"]["r"].as_u64(),
                            data["color"]["g"].as_u64(),
                            data["color"]["b"].as_u64(),
                        ) {
                            let mut state = state_for_cmd.lock().unwrap();
                            state.color = DeviceColor {
                                r: r as u8,
                                g: g as u8,
                                b: b as u8,
                            };
                        }
                    }
                }
            }
        });

        Ok(Self {
            sku: sku.to_string(),
            device_id: device_id.to_string(),
            state,
            cmd_port,
            scan_port,
        })
    }

    fn current_state(&self) -> DeviceStatus {
        self.state.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn simulated_device_responds_to_scan() {
    let device = SimulatedDevice::start("H6076", "AA:BB:CC:DD:EE:FF:00:11")
        .await
        .unwrap();

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let scan_msg = json!({
        "msg": {
            "cmd": "scan",
            "data": { "account_topic": "reserve" }
        }
    });

    client
        .send_to(
            scan_msg.to_string().as_bytes(),
            format!("127.0.0.1:{}", device.scan_port),
        )
        .await
        .unwrap();

    let mut buf = [0u8; 4096];
    let (len, _) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.recv_from(&mut buf),
    )
    .await
    .unwrap()
    .unwrap();

    let response: serde_json::Value = serde_json::from_slice(&buf[..len]).unwrap();

    assert_eq!(response["msg"]["cmd"], "scan");
    assert_eq!(response["msg"]["data"]["sku"], "H6076");
    assert_eq!(response["msg"]["data"]["device"], "AA:BB:CC:DD:EE:FF:00:11");
}

#[tokio::test]
async fn simulated_device_responds_to_status_query() {
    let device = SimulatedDevice::start("H6076", "AA:BB:CC:DD:EE:FF:00:11")
        .await
        .unwrap();

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let status_msg = json!({
        "msg": {
            "cmd": "devStatus",
            "data": {}
        }
    });

    client
        .send_to(
            status_msg.to_string().as_bytes(),
            format!("127.0.0.1:{}", device.cmd_port),
        )
        .await
        .unwrap();

    let mut buf = [0u8; 4096];
    let (len, _) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.recv_from(&mut buf),
    )
    .await
    .unwrap()
    .unwrap();

    let response: serde_json::Value = serde_json::from_slice(&buf[..len]).unwrap();

    assert_eq!(response["msg"]["cmd"], "devStatus");
    assert_eq!(response["msg"]["data"]["onOff"], 1);
    assert_eq!(response["msg"]["data"]["brightness"], 100);
    assert_eq!(response["msg"]["data"]["color"]["r"], 255);
    assert_eq!(response["msg"]["data"]["color"]["g"], 128);
    assert_eq!(response["msg"]["data"]["color"]["b"], 0);
}

#[tokio::test]
async fn simulated_device_accepts_turn_command() {
    let device = SimulatedDevice::start("H6076", "AA:BB:CC:DD:EE:FF:00:11")
        .await
        .unwrap();

    assert!(device.current_state().on);

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let turn_off = json!({
        "msg": {
            "cmd": "turn",
            "data": { "value": 0 }
        }
    });

    client
        .send_to(
            turn_off.to_string().as_bytes(),
            format!("127.0.0.1:{}", device.cmd_port),
        )
        .await
        .unwrap();

    // Give the spawned task time to process
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert!(!device.current_state().on);
}

#[tokio::test]
async fn simulated_device_accepts_color_command() {
    let device = SimulatedDevice::start("H6076", "AA:BB:CC:DD:EE:FF:00:11")
        .await
        .unwrap();

    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let set_color = json!({
        "msg": {
            "cmd": "colorwc",
            "data": {
                "color": { "r": 0, "g": 255, "b": 100 },
                "colorTemInKelvin": 0
            }
        }
    });

    client
        .send_to(
            set_color.to_string().as_bytes(),
            format!("127.0.0.1:{}", device.cmd_port),
        )
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let state = device.current_state();
    assert_eq!(state.color.r, 0);
    assert_eq!(state.color.g, 255);
    assert_eq!(state.color.b, 100);
}
