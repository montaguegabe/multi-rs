//! `multi service`: a loopback-only JSON service exposing the project
//! inspection API to local tools.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::app_api::{
    get_history_group_diff, get_project_detail, get_projects_summary, refresh_projects_summary,
    HistoryCommitRef,
};
use crate::error::{Error, Result};

fn json_response(status: u16, payload: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    let header = Header::from_bytes("Content-Type", "application/json").expect("valid header");
    Response::from_data(body)
        .with_status_code(status)
        .with_header(header)
}

fn string_list(body: &Value, key: &str) -> std::result::Result<Vec<String>, String> {
    let items = body
        .get(key)
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let Value::Array(items) = items else {
        return Err(format!("{key} must be a list of strings."));
    };
    items
        .into_iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{key} must be a list of strings."))
        })
        .collect()
}

/// Handle one request; returns `(status, payload)`.
pub fn handle(method: &Method, path: &str, body: &str) -> (u16, Value) {
    if *method == Method::Get {
        if path == "/health" {
            return (200, json!({"ok": true}));
        }
        return (404, json!({"error": format!("Unknown route: {path}")}));
    }
    if *method != Method::Post {
        return (404, json!({"error": format!("Unknown route: {path}")}));
    }
    let body: Value = match serde_json::from_str(if body.trim().is_empty() { "{}" } else { body }) {
        Ok(value) => value,
        Err(err) => return (400, json!({"error": err.to_string()})),
    };
    let result: std::result::Result<Value, String> = match path {
        "/v1/projects/summary" => string_list(&body, "repoPaths")
            .map(|paths| json!({"projects": get_projects_summary(&paths)})),
        "/v1/projects/status" => string_list(&body, "repoPaths")
            .map(|paths| json!({"projects": refresh_projects_summary(&paths)})),
        "/v1/project/detail" => match body.get("repoPath").and_then(Value::as_str) {
            Some(repo_path) if !repo_path.is_empty() => {
                serde_json::to_value(get_project_detail(std::path::Path::new(repo_path)))
                    .map_err(|e| e.to_string())
            }
            _ => Err("repoPath must be a non-empty string.".to_string()),
        },
        "/v1/history/group-diff" => {
            let commits = body
                .get("commits")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            if !commits.is_array() {
                Err("commits must be a list.".to_string())
            } else {
                serde_json::from_value::<Vec<HistoryCommitRef>>(commits)
                    .map_err(|e| e.to_string())
                    .map(|commits| json!({"repoDiffs": get_history_group_diff(&commits)}))
            }
        }
        _ => return (404, json!({"error": format!("Unknown route: {path}")})),
    };
    match result {
        Ok(payload) => (200, payload),
        Err(message) => (400, json!({"error": message})),
    }
}

fn respond(mut request: Request) {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let (status, payload) = handle(&method, &path, &body);
    log::debug!("multi service: {method} {path} -> {status}");
    let _ = request.respond(json_response(status, &payload));
}

/// Bind the service. Only loopback hosts are accepted.
pub fn bind(host: &str, port: u16) -> Result<Server> {
    let ip: IpAddr = match host {
        "localhost" | "127.0.0.1" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        _ => {
            return Err(Error::Other(
                "Service host must remain loopback-only.".to_string(),
            ))
        }
    };
    Server::http(SocketAddr::new(ip, port))
        .map_err(|e| Error::Other(format!("Failed to start service: {e}")))
}

/// Serve forever on an already-bound server.
pub fn serve(server: Server) {
    for request in server.incoming_requests() {
        std::thread::spawn(move || respond(request));
    }
}

/// Bind, print the `{"event":"ready","port":N}` line, and serve.
pub fn run(host: &str, port: u16) -> Result<()> {
    let server = bind(host, port)?;
    let actual_port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .unwrap_or(port);
    println!("{}", json!({"event": "ready", "port": actual_port}));
    serve(server);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(handle(&Method::Get, "/health", "").0, 200);
        assert_eq!(handle(&Method::Get, "/nope", "").0, 404);
        assert_eq!(
            handle(
                &Method::Post,
                "/v1/projects/summary",
                "{\"repoPaths\": [1]}"
            )
            .0,
            400
        );
        assert_eq!(handle(&Method::Post, "/v1/project/detail", "{}").0, 400);
        assert_eq!(
            handle(&Method::Post, "/v1/projects/summary", "not json").0,
            400
        );
        let (status, payload) =
            handle(&Method::Post, "/v1/history/group-diff", "{\"commits\": []}");
        assert_eq!(status, 200);
        assert_eq!(payload["repoDiffs"], json!([]));
        assert!(bind("0.0.0.0", 0).is_err());
    }
}
