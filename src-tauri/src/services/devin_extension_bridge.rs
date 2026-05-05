use crate::commands::devin_commands::import_devin_auth1_token_account;
use crate::repository::DataStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const BRIDGE_PORTS: &[u16] = &[19876, 19877, 19878, 19879, 19880];
const MAX_REQUEST_SIZE: usize = 64 * 1024;
const EXTENSION_HEADER: &str = "devin-auth1";

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevinAuth1ImportRequest {
    auth1_token: Option<String>,
    token: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DevinAuth1ImportEvent {
    success: bool,
    status: String,
    email: Option<String>,
    message: String,
}

pub fn start_devin_extension_bridge(app_handle: AppHandle, store: Arc<DataStore>) {
    tauri::async_runtime::spawn(async move {
        let (listener, port) = match bind_bridge_listener().await {
            Ok(bound) => bound,
            Err(error) => {
                eprintln!("[DevinExtensionBridge] Failed to start: {}", error);
                return;
            }
        };

        let _ = app_handle.emit(
            "devin-extension-bridge-ready",
            json!({ "port": port, "ports": BRIDGE_PORTS }),
        );
        println!("[DevinExtensionBridge] Listening on 127.0.0.1:{}", port);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let app_handle = app_handle.clone();
                    let store = store.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream, app_handle, store).await {
                            eprintln!("[DevinExtensionBridge] Request failed: {}", error);
                        }
                    });
                }
                Err(error) => {
                    eprintln!("[DevinExtensionBridge] Accept failed: {}", error);
                }
            }
        }
    });
}

async fn bind_bridge_listener() -> Result<(TcpListener, u16), String> {
    for port in BRIDGE_PORTS {
        match TcpListener::bind(("127.0.0.1", *port)).await {
            Ok(listener) => return Ok((listener, *port)),
            Err(_) => continue,
        }
    }
    Err(format!("ports {:?} are unavailable", BRIDGE_PORTS))
}

async fn handle_connection(
    mut stream: TcpStream,
    app_handle: AppHandle,
    store: Arc<DataStore>,
) -> Result<(), String> {
    let request =
        match tokio::time::timeout(Duration::from_secs(5), read_http_request(&mut stream)).await {
            Ok(result) => result?,
            Err(_) => return Err("request timed out".to_string()),
        };

    let origin = request.headers.get("origin").cloned();
    if !is_origin_allowed(origin.as_deref()) {
        write_json_response(
            &mut stream,
            403,
            "Forbidden",
            &json!({ "success": false, "message": "Origin is not allowed" }),
            None,
        )
        .await?;
        return Ok(());
    }

    if request.method == "OPTIONS" {
        write_options_response(&mut stream, origin.as_deref()).await?;
        return Ok(());
    }

    if request.method == "GET" && request.path == "/health" {
        write_json_response(
            &mut stream,
            200,
            "OK",
            &json!({ "success": true, "service": "devin-auth1-bridge" }),
            origin.as_deref(),
        )
        .await?;
        return Ok(());
    }

    if request.method != "POST" || request.path != "/devin-auth1-token" {
        write_json_response(
            &mut stream,
            404,
            "Not Found",
            &json!({ "success": false, "message": "Endpoint not found" }),
            origin.as_deref(),
        )
        .await?;
        return Ok(());
    }

    if request
        .headers
        .get("x-wam-extension")
        .map(|value| value.as_str())
        != Some(EXTENSION_HEADER)
    {
        write_json_response(
            &mut stream,
            403,
            "Forbidden",
            &json!({ "success": false, "message": "Extension header is missing" }),
            origin.as_deref(),
        )
        .await?;
        return Ok(());
    }

    let payload: DevinAuth1ImportRequest = serde_json::from_slice(&request.body)
        .map_err(|error| format!("invalid JSON payload: {}", error))?;
    let token = payload
        .auth1_token
        .or(payload.token)
        .unwrap_or_default()
        .trim()
        .to_string();

    if !token.starts_with("auth1_") {
        write_json_response(
            &mut stream,
            400,
            "Bad Request",
            &json!({ "success": false, "message": "Invalid auth1 token" }),
            origin.as_deref(),
        )
        .await?;
        return Ok(());
    }

    let import_result = import_devin_auth1_token_account(
        token,
        None,
        None,
        Vec::new(),
        None,
        Some(true),
        &store,
        true,
    )
    .await;

    let response = import_result_to_response(import_result);
    let event = response_to_event(&response);
    let _ = app_handle.emit("devin-auth1-token-imported", event);

    write_json_response(&mut stream, 200, "OK", &response, origin.as_deref()).await?;
    Ok(())
}

fn import_result_to_response(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            let already_exists = value
                .get("already_exists")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let requires_org_selection = value
                .get("requires_org_selection")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let success = value
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let email = value.get("email").and_then(|value| value.as_str());
            let status = if already_exists {
                "already_exists"
            } else if requires_org_selection {
                "requires_org_selection"
            } else if success {
                "imported"
            } else {
                "error"
            };
            let message = value
                .get("message")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
                .unwrap_or_else(|| match status {
                    "already_exists" => "Account already exists".to_string(),
                    "imported" => "Account imported".to_string(),
                    "requires_org_selection" => "Organization selection is required".to_string(),
                    _ => "Import failed".to_string(),
                });

            json!({
                "success": success,
                "status": status,
                "alreadyExists": already_exists,
                "requiresOrgSelection": requires_org_selection,
                "email": email,
                "message": message,
            })
        }
        Err(error) => json!({
            "success": false,
            "status": "error",
            "message": error,
        }),
    }
}

fn response_to_event(response: &Value) -> DevinAuth1ImportEvent {
    DevinAuth1ImportEvent {
        success: response
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        status: response
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("error")
            .to_string(),
        email: response
            .get("email")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        message: response
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("Import failed")
            .to_string(),
    }
}

async fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut data = Vec::new();
    let mut buffer = [0_u8; 4096];

    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }

        data.extend_from_slice(&buffer[..read]);
        if data.len() > MAX_REQUEST_SIZE {
            return Err("request is too large".to_string());
        }

        if let Some(header_end) = find_header_end(&data) {
            let headers = parse_headers(&data[..header_end])?;
            let content_length = headers
                .get("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if content_length > MAX_REQUEST_SIZE {
                return Err("request body is too large".to_string());
            }
            if data.len() >= header_end + content_length {
                return build_http_request(data, header_end, content_length);
            }
        }
    }

    Err("incomplete HTTP request".to_string())
}

fn build_http_request(
    data: Vec<u8>,
    header_end: usize,
    content_length: usize,
) -> Result<HttpRequest, String> {
    let header_text = String::from_utf8_lossy(&data[..header_end]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| "missing request line".to_string())?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| "missing method".to_string())?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| "missing path".to_string())?
        .split('?')
        .next()
        .unwrap_or("")
        .to_string();
    let headers = parse_headers(&data[..header_end])?;
    let body_start = header_end;
    let body_end = body_start + content_length;
    let body = data[body_start..body_end].to_vec();

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn parse_headers(data: &[u8]) -> Result<HashMap<String, String>, String> {
    let header_text = String::from_utf8_lossy(data);
    let mut headers = HashMap::new();

    for line in header_text.split("\r\n").skip(1) {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    Ok(headers)
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn is_origin_allowed(origin: Option<&str>) -> bool {
    match origin {
        Some(value) => value.starts_with("chrome-extension://"),
        None => true,
    }
}

async fn write_options_response(
    stream: &mut TcpStream,
    origin: Option<&str>,
) -> Result<(), String> {
    let cors_origin = origin.unwrap_or("*");
    let response = format!(
        "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: {}\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, X-WAM-Extension\r\nAccess-Control-Max-Age: 600\r\nVary: Origin\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        cors_origin
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| error.to_string())
}

async fn write_json_response(
    stream: &mut TcpStream,
    status_code: u16,
    status_text: &str,
    body: &Value,
    origin: Option<&str>,
) -> Result<(), String> {
    let body = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    let cors_origin = origin.unwrap_or("*");
    let response_head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: {}\r\nVary: Origin\r\nConnection: close\r\n\r\n",
        status_code,
        status_text,
        body.len(),
        cors_origin
    );
    stream
        .write_all(response_head.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stream
        .write_all(&body)
        .await
        .map_err(|error| error.to_string())
}
