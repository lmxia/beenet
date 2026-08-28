use serde::Deserialize;
use serde_json::{json, Value};

const API_BASE: &str = "http://cloud.hyperos.online/api";

pub struct CloudClient;

#[derive(Deserialize)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
}

#[derive(Deserialize)]
pub struct CloudUser {
    pub email: String,
}

#[derive(Deserialize)]
pub struct DevicePoll {
    pub status: String,
    pub token: Option<String>,
    pub user: Option<CloudUser>,
}

#[derive(Deserialize)]
pub struct BootstrapToken {
    pub id: String,
    pub token_value: String,
}

impl CloudClient {
    pub fn start_device_login() -> Result<DeviceStart, String> {
        post_json("/v1/auth/device/start", json!({}), None)
    }

    pub fn poll_device_login(device_code: &str) -> Result<DevicePoll, String> {
        post_json(
            "/v1/auth/device/poll",
            json!({ "device_code": device_code }),
            None,
        )
    }

    pub fn mint_bootstrap_token(token: &str) -> Result<BootstrapToken, String> {
        post_json("/v1/me/bootstrap-tokens", json!({}), Some(token))
    }

    pub fn claim_worker(
        token: &str,
        peer_id: &str,
        name: &str,
        region: &str,
        bootstrap_token_id: Option<&str>,
    ) -> Result<(), String> {
        let mut body = json!({ "peer_id": peer_id });
        if !name.trim().is_empty() {
            body["name"] = json!(name.trim());
        }
        if !region.trim().is_empty() {
            body["region"] = json!(region.trim());
        }
        if let Some(id) = bootstrap_token_id {
            body["bootstrap_token_id"] = json!(id);
        }
        let _: Value = post_json("/v1/me/workers", body, Some(token))?;
        Ok(())
    }

    pub fn points(token: &str) -> Result<i64, String> {
        #[derive(Deserialize)]
        struct Points {
            points: i64,
        }
        let value: Points = get_json("/v1/me/points", Some(token))?;
        Ok(value.points)
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())
}

fn post_json<T: for<'de> Deserialize<'de>>(
    path: &str,
    body: Value,
    token: Option<&str>,
) -> Result<T, String> {
    send("POST", path, Some(body), token)
}

fn get_json<T: for<'de> Deserialize<'de>>(path: &str, token: Option<&str>) -> Result<T, String> {
    send("GET", path, None, token)
}

fn send<T: for<'de> Deserialize<'de>>(
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> Result<T, String> {
    let url = format!("{API_BASE}{path}");
    let mut request = match method {
        "GET" => client()?.get(&url),
        _ => client()?.post(&url).json(&body.unwrap_or(json!({}))),
    };
    request = request.header("Accept", "application/json");
    if let Some(token) = token.filter(|value| !value.is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request.send().map_err(|error| error.to_string())?;
    let status = response.status();
    let text = response.text().map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(cloud_message(&text));
    }
    serde_json::from_str(&text).map_err(|_| cloud_message(&text))
}

fn cloud_message(text: &str) -> String {
    #[derive(Deserialize)]
    struct Envelope {
        error: Option<Body>,
    }
    #[derive(Deserialize)]
    struct Body {
        message: Option<String>,
    }
    if let Ok(envelope) = serde_json::from_str::<Envelope>(text) {
        if let Some(message) = envelope.error.and_then(|body| body.message) {
            if message.contains("unused bootstrap token") {
                return "上一枚入网凭证还在有效期内。请再点一次开始贡献。".into();
            }
            if !message.is_empty() {
                return message;
            }
        }
    }
    if text.trim().is_empty() {
        "Cloud 请求失败".into()
    } else {
        text.chars().take(180).collect()
    }
}
