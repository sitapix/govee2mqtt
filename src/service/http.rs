use crate::service::coordinator::Coordinator;
use crate::service::device::{Device, DeviceState};
use crate::service::hass::topic_safe_string;
use crate::service::state::{sort_and_dedup_scenes, StateHandle};
use crate::undoc_api::LightEffectCategory;
use anyhow::Context;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tower_http::services::ServeDir;

const INDEX_TEMPLATE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/index.html"));

fn response_with_code<T: ToString + std::fmt::Display>(code: StatusCode, err: T) -> Response {
    if !code.is_success() {
        log::error!("err: {err:#}");
    }

    let mut response = Json(serde_json::json!({
        "code": code.as_u16(),
        "msg": format!("{err:#}")
    }))
    .into_response();
    *response.status_mut() = code;
    response
}

fn generic<T: ToString + std::fmt::Display>(err: T) -> Response {
    response_with_code(StatusCode::INTERNAL_SERVER_ERROR, err)
}

fn not_found<T: ToString + std::fmt::Display>(err: T) -> Response {
    response_with_code(StatusCode::NOT_FOUND, err)
}

fn bad_request<T: ToString + std::fmt::Display>(err: T) -> Response {
    response_with_code(StatusCode::BAD_REQUEST, err)
}

fn ingress_only_enabled() -> bool {
    std::env::var("GOVEE_HTTP_INGRESS_ONLY")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn ingress_base_href(headers: &HeaderMap) -> String {
    match headers
        .get("X-Ingress-Path")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(path) => format!("{}/", path.trim_end_matches('/')),
        None => "/".to_string(),
    }
}

async fn require_ingress_source(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if addr.ip() != IpAddr::V4(Ipv4Addr::new(172, 30, 32, 2)) {
        return response_with_code(StatusCode::FORBIDDEN, "access denied");
    }

    next.run(request).await
}

fn get_auth_token() -> Option<String> {
    std::env::var("GOVEE_HTTP_AUTH_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
}

async fn require_auth_token(
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(expected) = get_auth_token() else {
        return next.run(request).await;
    };

    // Skip auth for the health endpoint
    if request.uri().path() == "/api/health" {
        return next.run(request).await;
    }

    // Check Authorization: Bearer <token> header
    let authorized = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| token.trim() == expected.trim())
        .unwrap_or(false)
        // Also check ?token= query parameter (for browser/WebSocket use)
        || request
            .uri()
            .query()
            .and_then(|q| {
                q.split('&')
                    .find_map(|pair| pair.strip_prefix("token="))
            })
            .map(|token| token == expected.trim())
            .unwrap_or(false);

    if authorized {
        next.run(request).await
    } else {
        response_with_code(StatusCode::UNAUTHORIZED, "invalid or missing auth token")
    }
}

async fn resolve_device_for_control(
    state: &StateHandle,
    id: &str,
) -> Result<Coordinator, Response> {
    state
        .resolve_device_for_control(&id)
        .await
        .map_err(not_found)
}

async fn resolve_device_read_only(state: &StateHandle, id: &str) -> Result<Device, Response> {
    state.resolve_device_read_only(&id).await.map_err(not_found)
}

/// Returns a json array of device information
async fn list_devices(State(state): State<StateHandle>) -> Result<Response, Response> {
    let mut devices = state.devices().await;
    devices.sort_by_key(|d| (d.room_name().map(|name| name.to_string()), d.name()));

    #[derive(Serialize)]
    struct DeviceItem {
        pub sku: String,
        pub id: String,
        pub safe_id: String,
        pub name: String,
        pub room: Option<String>,
        pub ip: Option<IpAddr>,
        pub state: Option<DeviceState>,
    }

    let devices: Vec<_> = devices
        .into_iter()
        .map(|d| DeviceItem {
            name: d.name(),
            room: d.room_name().map(|r| r.to_string()),
            ip: d.ip_addr(),
            state: d.device_state(),
            safe_id: topic_safe_string(&d.id),
            sku: d.sku,
            id: d.id,
        })
        .collect();

    Ok(Json(devices).into_response())
}

/// Turns on a given device
async fn device_power_on(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_power_on(&device, true)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Turns off a given device
async fn device_power_off(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_power_on(&device, false)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Sets the brightness level of a given device
async fn device_set_brightness(
    State(state): State<StateHandle>,
    Path((id, level)): Path<(String, u8)>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_brightness(&device, level)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Sets the color temperature of a given device
async fn device_set_color_temperature(
    State(state): State<StateHandle>,
    Path((id, kelvin)): Path<(String, u32)>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_color_temperature(&device, kelvin)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Sets the RGB color of a given device
async fn device_set_color(
    State(state): State<StateHandle>,
    Path((id, color)): Path<(String, String)>,
) -> Result<Response, Response> {
    let color = csscolorparser::parse(&color)
        .map_err(|err| bad_request(format!("error parsing color '{color}': {err}")))?;
    let [r, g, b, _a] = color.to_rgba8();

    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_color_rgb(&device, r, g, b)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Activates the named scene for a given device
async fn device_set_scene(
    State(state): State<StateHandle>,
    Path((id, scene)): Path<(String, String)>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    state
        .device_set_scene(&device, &scene)
        .await
        .map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

/// Returns a JSON array of the available scene names for a given device
/// Send arbitrary ptReal command payloads to a device over LAN.
/// Body: {"command": ["base64_payload1", "base64_payload2"]}
async fn device_send_ptreal(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, Response> {
    let device = resolve_device_for_control(&state, &id).await?;

    let commands: Vec<String> = body
        .get("command")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| bad_request("body must have a 'command' array of base64 strings"))?;

    if commands.is_empty() {
        return Err(bad_request("command array is empty"));
    }

    if let Some(lan_dev) = &device.lan_device {
        log::info!("Sending ptReal to {device} via LAN ({} commands)", commands.len());
        lan_dev
            .send_real(commands)
            .await
            .map_err(generic)?;
    } else if let Some(iot) = state.get_iot_client().await {
        if let Some(info) = &device.undoc_device_info {
            log::info!("Sending ptReal to {device} via IoT ({} commands)", commands.len());
            iot.send_real(&info.entry, commands)
                .await
                .map_err(generic)?;
        } else {
            return Err(generic("device has no IoT info"));
        }
    } else {
        return Err(generic("no LAN or IoT API available for ptReal"));
    }

    Ok(response_with_code(StatusCode::OK, "ok"))
}

async fn device_list_scenes(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_read_only(&state, &id).await?;

    let scenes = state.device_list_scenes(&device).await.map_err(generic)?;

    Ok(Json(scenes).into_response())
}

fn scene_names_from_undoc_categories(categories: &[LightEffectCategory]) -> Vec<String> {
    let mut names = vec![];
    for category in categories {
        for scene in &category.scenes {
            if scene
                .light_effects
                .iter()
                .any(|effect| effect.scene_code != 0)
            {
                names.push(scene.scene_name.clone());
            }
        }
    }
    sort_and_dedup_scenes(names)
}

async fn platform_scene_capability_names(
    client: &crate::platform_api::GoveeApiClient,
    info: &crate::platform_api::HttpDeviceInfo,
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let mut result = BTreeMap::new();

    for instance in ["diyScene", "lightScene", "nightlightScene", "snapshot"] {
        let names = client.list_capability_names(info, instance).await?;
        if !names.is_empty() {
            result.insert(instance.to_string(), names);
        }
    }

    Ok(result)
}

#[derive(Serialize)]
struct DeviceInspectResponse {
    sku: String,
    id: String,
    name: String,
    room: Option<String>,
    current_state: Option<DeviceState>,
    active_scene: Option<String>,
    active_scene_instance: Option<String>,
    platform_device_info: Option<crate::platform_api::HttpDeviceInfo>,
    platform_device_info_error: Option<String>,
    platform_state: Option<crate::platform_api::HttpDeviceState>,
    platform_state_error: Option<String>,
    platform_scene_names: Option<Vec<String>>,
    platform_scene_names_error: Option<String>,
    platform_scene_capability_names: Option<BTreeMap<String, Vec<String>>>,
    platform_scene_capability_names_error: Option<String>,
    platform_music_mode_names: Option<Vec<String>>,
    platform_music_mode_names_error: Option<String>,
    undocumented_scene_categories: Option<Vec<LightEffectCategory>>,
    undocumented_scene_categories_error: Option<String>,
    undocumented_scene_names: Option<Vec<String>>,
    merged_scene_names: Option<Vec<String>>,
    merged_scene_names_error: Option<String>,
}

async fn inspect_device(
    State(state): State<StateHandle>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let device = resolve_device_read_only(&state, &id).await?;

    let mut platform_device_info = device.http_device_info.clone();
    let mut platform_device_info_error = None;
    let mut platform_state = None;
    let mut platform_state_error = None;
    let mut platform_scene_names = None;
    let mut platform_scene_names_error = None;
    let mut platform_scene_capability_names_value = None;
    let mut platform_scene_capability_names_error = None;
    let mut platform_music_mode_names = None;
    let mut platform_music_mode_names_error = None;

    if let Some(client) = state.get_platform_client().await {
        match client.get_device_by_id(&device.id).await {
            Ok(info) => {
                platform_device_info = Some(info.clone());

                match client.get_device_state(&info).await {
                    Ok(raw_state) => {
                        platform_state = Some(raw_state);
                    }
                    Err(err) => {
                        platform_state_error = Some(format!("{err:#}"));
                    }
                }

                match client.list_scene_names(&info).await {
                    Ok(names) => {
                        platform_scene_names = Some(names);
                    }
                    Err(err) => {
                        platform_scene_names_error = Some(format!("{err:#}"));
                    }
                }

                match platform_scene_capability_names(&client, &info).await {
                    Ok(names) => {
                        platform_scene_capability_names_value = Some(names);
                    }
                    Err(err) => {
                        platform_scene_capability_names_error = Some(format!("{err:#}"));
                    }
                }

                match client.list_music_mode_names(&info) {
                    Ok(names) => {
                        platform_music_mode_names = Some(names);
                    }
                    Err(err) => {
                        platform_music_mode_names_error = Some(format!("{err:#}"));
                    }
                }
            }
            Err(err) => {
                platform_device_info_error = Some(format!("{err:#}"));
            }
        }
    }

    let (
        undocumented_scene_categories,
        undocumented_scene_categories_error,
        undocumented_scene_names,
    ) = match crate::undoc_api::GoveeUndocumentedApi::get_scenes_for_device(&device.sku).await {
        Ok(categories) => {
            let names = scene_names_from_undoc_categories(&categories);
            (Some(categories), None, Some(names))
        }
        Err(err) => (None, Some(format!("{err:#}")), None),
    };

    let (merged_scene_names, merged_scene_names_error) =
        match state.device_list_scenes(&device).await {
            Ok(names) => (Some(names), None),
            Err(err) => (None, Some(format!("{err:#}"))),
        };

    Ok(Json(DeviceInspectResponse {
        sku: device.sku.clone(),
        id: device.id.clone(),
        name: device.name(),
        room: device.room_name(),
        current_state: device.device_state(),
        active_scene: device.active_scene_name().map(str::to_string),
        active_scene_instance: device.active_scene_instance().map(str::to_string),
        platform_device_info,
        platform_device_info_error,
        platform_state,
        platform_state_error,
        platform_scene_names,
        platform_scene_names_error,
        platform_scene_capability_names: platform_scene_capability_names_value,
        platform_scene_capability_names_error,
        platform_music_mode_names,
        platform_music_mode_names_error,
        undocumented_scene_categories,
        undocumented_scene_categories_error,
        undocumented_scene_names,
        merged_scene_names,
        merged_scene_names_error,
    })
    .into_response())
}

async fn list_one_clicks(State(state): State<StateHandle>) -> Result<Response, Response> {
    let undoc = state
        .get_undoc_client()
        .await
        .ok_or_else(|| anyhow::anyhow!("Undoc API client is not available"))
        .map_err(generic)?;
    let items = undoc.parse_one_clicks().await.map_err(generic)?;

    Ok(Json(items).into_response())
}

async fn activate_one_click(
    State(state): State<StateHandle>,
    Path(name): Path<String>,
) -> Result<Response, Response> {
    let undoc = state
        .get_undoc_client()
        .await
        .ok_or_else(|| anyhow::anyhow!("Undoc API client is not available"))
        .map_err(generic)?;
    let items = undoc.parse_one_clicks().await.map_err(generic)?;
    let item = items
        .iter()
        .find(|item| item.name == name)
        .ok_or_else(|| anyhow::anyhow!("didn't find item {name}"))
        .map_err(not_found)?;

    let iot = state
        .get_iot_client()
        .await
        .ok_or_else(|| anyhow::anyhow!("AWS IoT client is not available"))
        .map_err(generic)?;

    iot.activate_one_click(&item).await.map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "ok"))
}

async fn get_config() -> Response {
    let config = crate::service::device_config::current_config();
    Json(&*config).into_response()
}

async fn put_config(Json(body): Json<serde_json::Value>) -> Result<Response, Response> {
    let config: crate::service::device_config::DeviceConfigFile =
        serde_json::from_value(body)
            .map_err(|e| bad_request(format!("Invalid config JSON: {e}")))?;

    crate::service::device_config::save_config(&config).map_err(generic)?;

    Ok(response_with_code(StatusCode::OK, "Config saved"))
}

async fn health_check(State(state): State<StateHandle>) -> Response {
    let devices = state.devices().await;
    let now = chrono::Utc::now();
    let mut online = 0u32;
    for device in &devices {
        let is_online = device.is_online(now);
        if is_online {
            online += 1;
        }
    }

    // Process memory usage (RSS) on Linux/macOS
    let memory_mb = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| (pages * 4096) / (1024 * 1024)) // pages to MB
        .unwrap_or(0);

    Json(serde_json::json!({
        "status": "ok",
        "version": crate::version_info::govee_version(),
        "devices": devices.len(),
        "devices_online": online,
        "push": {
            "connected": state.push_connected.load(std::sync::atomic::Ordering::Relaxed),
            "events_received": state.push_event_count.load(std::sync::atomic::Ordering::Relaxed),
        },
        "process": {
            "memory_mb": memory_mb,
            "uptime_sec": std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        },
    }))
    .into_response()
}

async fn api_logs() -> Response {
    let logs = crate::service::log_capture::recent_logs();
    Json(logs).into_response()
}

async fn ws_logs(ws: axum::extract::WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_log_websocket)
}

async fn handle_log_websocket(mut socket: axum::extract::ws::WebSocket) {
    let mut rx = crate::service::log_capture::subscribe();

    while let Ok(entry) = rx.recv().await {
        let Ok(json) = serde_json::to_string(&entry) else {
            continue;
        };
        if socket
            .send(axum::extract::ws::Message::Text(json.into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn serve_index(headers: HeaderMap) -> Html<String> {
    Html(INDEX_TEMPLATE.replace("__BASE_HREF__", &ingress_base_href(&headers)))
}

fn build_router(state: StateHandle, ingress_only: bool) -> Router {
    let mut app = Router::new()
        .route("/", get(serve_index))
        .route("/assets/index.html", get(serve_index))
        .route("/api/health", get(health_check))
        .route("/api/logs", get(api_logs))
        .route("/api/ws/logs", get(ws_logs))
        .route("/api/devices", get(list_devices))
        .route("/api/device/{id}/power/on", post(device_power_on))
        .route("/api/device/{id}/power/off", post(device_power_off))
        .route(
            "/api/device/{id}/brightness/{level}",
            post(device_set_brightness),
        )
        .route(
            "/api/device/{id}/colortemp/{kelvin}",
            post(device_set_color_temperature),
        )
        .route("/api/device/{id}/inspect", get(inspect_device))
        .route("/api/device/{id}/color/{color}", post(device_set_color))
        .route("/api/device/{id}/scene/{scene}", post(device_set_scene))
        .route("/api/device/{id}/ptreal", post(device_send_ptreal))
        .route("/api/device/{id}/scenes", get(device_list_scenes))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/oneclicks", get(list_one_clicks))
        .route("/api/oneclick/activate/{scene}", post(activate_one_click))
        .nest_service("/assets", ServeDir::new("assets"))
        .with_state(state);

    if ingress_only {
        app = app.layer(middleware::from_fn(require_ingress_source));
    }

    if get_auth_token().is_some() {
        app = app.layer(middleware::from_fn(require_auth_token));
    }

    app
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::util::ServiceExt;

    #[test]
    fn test_build_router() {
        // axum has a history of chaning the URL syntax across
        // semver bumps; while that is OK, the syntax changes
        // are not caught at compile time, so we need a runtime
        // check to verify that the syntax is still good.
        // This next line will panic if axum decides that
        // the syntax is bad.
        let _ = build_router(StateHandle::default(), false);
    }

    #[tokio::test]
    async fn index_uses_ingress_path_as_base_href() {
        let app = build_router(StateHandle::default(), false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("X-Ingress-Path", "/api/hassio_ingress/example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains(r#"/api/hassio_ingress/example/"#));
    }

    #[tokio::test]
    async fn mutating_routes_require_post() {
        let app = build_router(StateHandle::default(), false);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/device/test-device/power/on")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn inspect_route_exists() {
        let app = build_router(StateHandle::default(), false);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/device/test-device/inspect")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn inspect_route_reports_scene_parity_fields() {
        let state = StateHandle::default();
        {
            let mut device = state.device_mut("H6000", "test-device").await;
            device.set_http_device_info(crate::platform_api::HttpDeviceInfo {
                sku: "H6000".to_string(),
                device: "test-device".to_string(),
                device_name: "Desk Lamp".to_string(),
                device_type: crate::platform_api::DeviceType::Light,
                capabilities: vec![],
            });
        }

        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/device/test-device/inspect")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json = String::from_utf8(body.to_vec()).unwrap();
        assert!(json.contains("\"platform_scene_capability_names\""));
        assert!(json.contains("\"platform_music_mode_names\""));
        assert!(json.contains("\"merged_scene_names\""));
    }

    #[tokio::test]
    async fn inspect_route_accepts_url_safe_device_id() {
        let state = StateHandle::default();
        {
            let mut device = state.device_mut("H6000", "AA:BB:CC:DD:EE:FF:42:2A").await;
            device.set_http_device_info(crate::platform_api::HttpDeviceInfo {
                sku: "H6000".to_string(),
                device: "AA:BB:CC:DD:EE:FF:42:2A".to_string(),
                device_name: "Desk Lamp".to_string(),
                device_type: crate::platform_api::DeviceType::Light,
                capabilities: vec![],
            });
        }

        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/device/aa_bb_cc_dd_ee_ff_42_2a/inspect")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ingress_only_router_rejects_non_supervisor_requests() {
        let app = build_router(StateHandle::default(), true);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8123))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn ingress_only_router_allows_supervisor_requests() {
        let app = build_router(StateHandle::default(), true);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(ConnectInfo(SocketAddr::from(([172, 30, 32, 2], 8123))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_endpoint_returns_valid_json() {
        let state = StateHandle::default();
        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["version"].is_string());
        assert_eq!(json["devices"], 0);
        assert_eq!(json["devices_online"], 0);
    }

    #[tokio::test]
    async fn health_endpoint_counts_devices() {
        let state = StateHandle::default();
        {
            let mut device = state.device_mut("H6000", "AA:BB").await;
            device.set_http_device_info(crate::platform_api::HttpDeviceInfo {
                sku: "H6000".to_string(),
                device: "AA:BB".to_string(),
                device_name: "Test Lamp".to_string(),
                device_type: crate::platform_api::DeviceType::Light,
                capabilities: vec![],
            });
        }

        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["devices"], 1);
    }

    #[tokio::test]
    async fn devices_endpoint_returns_array() {
        let state = StateHandle::default();
        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/devices")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.is_array());
    }

    #[tokio::test]
    async fn nonexistent_device_returns_not_found() {
        let state = StateHandle::default();
        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/device/does-not-exist/scenes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_color_returns_bad_request() {
        let state = StateHandle::default();
        {
            let mut device = state.device_mut("H6000", "AA:BB").await;
            device.set_http_device_info(crate::platform_api::HttpDeviceInfo {
                sku: "H6000".to_string(),
                device: "AA:BB".to_string(),
                device_name: "Test".to_string(),
                device_type: crate::platform_api::DeviceType::Light,
                capabilities: vec![],
            });
        }

        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/device/AABB/color/not-a-color")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn invalid_api_route_returns_not_found() {
        let state = StateHandle::default();
        let app = build_router(state, false);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

pub async fn run_http_server(state: StateHandle, port: u16) -> anyhow::Result<()> {
    let app = build_router(state, ingress_only_enabled());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("run_http_server: binding to port {port}"))?;
    let addr = listener.local_addr()?;
    log::info!("http server addr is {addr:?}");
    if let Err(err) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        log::error!("http server stopped: {err:#}");
    }

    Ok(())
}
