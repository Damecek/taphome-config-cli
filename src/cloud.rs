//! TapHome cloud HTTP API: `login2` returns the per-location authorization token, `tunnelendpoints3`
//! the relay URLs. The Core protocol on a tunnel is the same as on the LAN (see docs/architecture.md).
use crate::client::{DEVICE_NAME, PLATFORM, SOFTWARE_VERSION};
use crate::errors::{CliError, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const DEFAULT_API: &str = "https://cloudapi.taphome.com/api/user/";
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// One location from `login2`. `authorization_token` is the secret the Core login hashes.
#[derive(Clone, PartialEq)]
pub struct Location {
    pub id: String,
    pub name: String,
    pub authorization_token: String,
}

// The token is a credential, so Debug leaves it out.
impl std::fmt::Debug for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Location")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

pub struct CloudLogin {
    pub location: Location,
    pub tunnels: Vec<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into()
}

/// `(status, body)`. A transport failure is `connection_failed`.
fn call(
    request: impl FnOnce(
        &ureq::Agent,
    ) -> std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<(u16, Value)> {
    let mut response = request(&agent()).map_err(|e| {
        CliError::new(
            "connection_failed",
            format!("cannot reach the TapHome cloud: {e}"),
        )
    })?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    Ok((
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    ))
}

/// `title: detail` of a problem+json body, or the plain text, cut to 200 characters.
fn problem(body: &Value) -> String {
    let text = match body {
        Value::Object(_) => [&body["title"], &body["detail"]]
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(": "),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    text.chars().take(200).collect()
}

/// Blocking HTTPS calls are fine here: nothing else runs on the runtime during a login.
pub fn login(
    api: &str,
    email: &str,
    password: &str,
    device_id: &str,
    wanted: Option<&str>,
) -> Result<CloudLogin> {
    let body = json!({"deviceId": device_id, "platform": PLATFORM, "softwareVersion": SOFTWARE_VERSION,
                      "email": email, "password": password, "languageCode": "en", "deviceName": DEVICE_NAME});
    let (status, reply) = call(|a| {
        a.post(format!("{api}login2"))
            .header("Content-Type", "application/json")
            .send(body.to_string())
    })?;
    if status != 200 {
        // Only these mean wrong credentials. 404, 408, 429 and 5xx are worth a retry or a config check.
        let code = if matches!(status, 400 | 401 | 403) {
            "login_failed"
        } else {
            "connection_failed"
        };
        return Err(CliError::new(
            code,
            format!(
                "the TapHome cloud refused the login (HTTP {status}): {}",
                problem(&reply)
            ),
        )
        .with_details(json!({"http_status": status})));
    }
    let locations = reply["myInfo"]["myLocations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let location = pick_location(&locations, wanted)?;
    let (status, reply) = call(|a| a.get(format!("{api}tunnelendpoints3")).call())?;
    // The Core login travels in the tunnel: never accept a plain ws:// relay from the real cloud.
    // A plain http:// API (TAPHOME_CLOUD_API in tests) may name a plain ws:// tunnel.
    let plain_ok = api.starts_with("http://");
    let tunnels: Vec<String> = reply["tunnels"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|u| u.as_str())
                .filter(|u| u.starts_with("wss://") || plain_ok && u.starts_with("ws://"))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if status != 200 || tunnels.is_empty() {
        return Err(CliError::new(
            "connection_failed",
            format!("the TapHome cloud sent no usable tunnel endpoint (HTTP {status})"),
        ));
    }
    Ok(CloudLogin { location, tunnels })
}

/// The only location, or the one whose id or name (case-insensitive) is `wanted`.
pub fn pick_location(locations: &[Value], wanted: Option<&str>) -> Result<Location> {
    let all: Vec<Location> = locations
        .iter()
        .map(|l| Location {
            id: l["locationId"].as_str().unwrap_or_default().to_lowercase(),
            name: l["name"].as_str().unwrap_or_default().to_string(),
            authorization_token: l["authorizationToken"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase(),
        })
        .filter(|l| !l.id.is_empty() && !l.authorization_token.is_empty())
        .collect();
    let names: Vec<&str> = all.iter().map(|l| l.name.as_str()).collect();
    let matching: Vec<&Location> = match wanted {
        Some(w) => all
            .iter()
            .filter(|l| l.id.eq_ignore_ascii_case(w) || l.name.to_lowercase() == w.to_lowercase())
            .collect(),
        None => all.iter().collect(),
    };
    match matching.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(CliError::new(
            "not_found",
            match wanted {
                Some(w) => format!("this TapHome account has no location '{w}'"),
                None => "this TapHome account has no location".to_string(),
            },
        )
        .with_details(json!({"locations": names}))),
        _ => Err(CliError::new(
            "ambiguous",
            "this TapHome account has several locations: set TAPHOME_LOCATION to the name or id of one",
        )
        .with_details(json!({"locations": names}))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locations() -> Vec<Value> {
        vec![
            json!({"locationId": "AAAA-1", "name": "Home", "authorizationToken": "TOKEN-A"}),
            json!({"locationId": "bbbb-2", "name": "Cottage", "authorizationToken": "token-b"}),
            json!({"locationId": "cccc-3", "name": "No token"}),
        ]
    }

    #[test]
    fn picks_by_name_or_id_and_lowercases_ids_and_tokens() {
        let home = pick_location(&locations(), Some("home")).unwrap();
        assert_eq!(
            (home.id.as_str(), home.authorization_token.as_str()),
            ("aaaa-1", "token-a")
        );
        assert_eq!(
            pick_location(&locations(), Some("BBBB-2")).unwrap().name,
            "Cottage"
        );
    }

    #[test]
    fn several_locations_need_a_choice_and_unknown_is_not_found() {
        assert_eq!(
            pick_location(&locations(), None).unwrap_err().code,
            "ambiguous"
        );
        assert_eq!(
            pick_location(&locations(), Some("Office"))
                .unwrap_err()
                .code,
            "not_found"
        );
        // A location without a token cannot be used, so it does not count.
        assert_eq!(pick_location(&locations()[..1], None).unwrap().name, "Home");
        assert_eq!(pick_location(&[], None).unwrap_err().code, "not_found");
    }

    #[test]
    fn problem_text_is_short() {
        assert_eq!(
            problem(&json!({"title": "Bad", "detail": "wrong password"})),
            "Bad: wrong password"
        );
        assert_eq!(problem(&json!("x".repeat(500))).len(), 200);
    }
}
