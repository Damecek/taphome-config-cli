//! JSON envelope and secret redaction. stdout carries exactly one document per invocation.
use crate::errors::CliError;
use serde_json::{json, Map, Value};

pub const REDACTED: &str = "<redacted>";

/// Redacted even with `--show-secrets`.
const NEVER_SHOWN: [&str; 6] = [
    "PasswordHashOrAuthorizationToken",
    "SessionId",
    "NewPasswordHash",
    "OldPasswordHash",
    "PinCodeHash",
    "CardCodeHash",
];
/// Redacted unless `--show-secrets`.
const SECRET_KEYS: [&str; 4] = ["AccessToken", "AuthorizationToken", "Token", "Passcode"];

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

pub fn success(command: &str, data: Value, warnings: &[String]) -> Value {
    json!({"ok": true, "command": command, "data": data, "warnings": warnings})
}

pub fn failure(command: &str, error: &CliError) -> Value {
    json!({"ok": false, "command": command, "error": error.to_json()})
}

pub struct Redactor {
    pub show_secrets: bool,
    /// Secrets the process knows (password, hash, salt, session id), removed wherever they appear as a whole string.
    pub values: Vec<String>,
}

fn is_property_map(key: &str) -> bool {
    key.ends_with("Properties") || key.ends_with("Parameters")
}

fn is_secret_property(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("password") || k.contains("token")
}

/// A property value for a diff or a printed text: masked when its key names a password or token and
/// the value is not empty. The envelope redactor only sees maps named `*Properties`, so commands
/// that print single values call this.
pub fn mask_value(key: &str, value: &str, show_secrets: bool) -> Value {
    if !show_secrets && !value.is_empty() && is_secret_property(key) {
        json!(REDACTED)
    } else {
        json!(value)
    }
}

fn is_empty(v: &Value) -> bool {
    matches!(v, Value::Null)
        || v.as_str() == Some("")
        || v.as_i64() == Some(0)
        || v.as_bool() == Some(false)
}

impl Redactor {
    pub fn redact(&self, value: &Value) -> Value {
        self.walk(value, false)
    }

    fn walk(&self, value: &Value, in_property_map: bool) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::new();
                for (k, v) in map {
                    out.insert(k.clone(), self.entry(k, v, in_property_map));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|v| self.walk(v, in_property_map))
                    .collect(),
            ),
            Value::String(s)
                if self
                    .values
                    .iter()
                    .any(|secret| !secret.is_empty() && secret == s) =>
            {
                json!(REDACTED)
            }
            other => other.clone(),
        }
    }

    fn entry(&self, key: &str, value: &Value, in_property_map: bool) -> Value {
        let hide = NEVER_SHOWN.contains(&key)
            || key.ends_with("PasswordHash")
            || (!self.show_secrets
                && (SECRET_KEYS.contains(&key) || (in_property_map && is_secret_property(key))));
        if hide && !is_empty(value) {
            return json!(REDACTED);
        }
        // A Packet Parser config travels as a JSON document inside a string.
        if key == "ConfigJson" {
            let parsed = value
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            if let Some(doc) = parsed.filter(|v| v.is_object() || v.is_array()) {
                return json!(self.walk(&doc, false).to_string());
            }
        }
        self.walk(value, is_property_map(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redactor(show: bool) -> Redactor {
        Redactor {
            show_secrets: show,
            values: vec!["s3cret".to_string()],
        }
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
        assert_eq!(from_hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(from_hex("0"), None);
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn redacts_hash_keys_always_and_passcode_and_token_unless_shown() {
        let doc = json!({"OldPasswordHash": "h1", "PinCodeHash": "h2", "CardCodeHash": "h3",
            "AdminPasswordHash": "h4", "Passcode": "1234", "Token": "t", "Tokens": "keep"});
        for show in [false, true] {
            let out = redactor(show).redact(&doc);
            for k in [
                "OldPasswordHash",
                "PinCodeHash",
                "CardCodeHash",
                "AdminPasswordHash",
            ] {
                assert_eq!(out[k], REDACTED, "{k} show={show}");
            }
            assert_eq!(out["Tokens"], "keep");
        }
        let hidden = redactor(false).redact(&doc);
        assert_eq!(
            (hidden["Passcode"].clone(), hidden["Token"].clone()),
            (json!(REDACTED), json!(REDACTED))
        );
        let shown = redactor(true).redact(&doc);
        assert_eq!(
            (shown["Passcode"].clone(), shown["Token"].clone()),
            (json!("1234"), json!("t"))
        );
    }

    #[test]
    fn redacts_keys_properties_values_and_config_json() {
        let doc = json!({
            "SessionId": "abc", "AccessToken": "tok", "Name": "keep", "Empty": "", "PasswordHash": "",
            "devices": [{"DeviceProperties": {"Password": "p", "ApiToken": "t", "Port": "80", "Note": "s3cret"}}],
            "ConfigJson": "{\"Modules\":[{\"DeviceProperties\":{\"Password\":\"p\",\"User\":\"u\"}}]}"
        });
        let hidden = redactor(false).redact(&doc);
        assert_eq!(hidden["SessionId"], REDACTED);
        assert_eq!(hidden["AccessToken"], REDACTED);
        assert_eq!(hidden["Name"], "keep");
        assert_eq!(hidden["PasswordHash"], "");
        assert_eq!(
            hidden["devices"][0]["DeviceProperties"]["Password"],
            REDACTED
        );
        assert_eq!(
            hidden["devices"][0]["DeviceProperties"]["ApiToken"],
            REDACTED
        );
        assert_eq!(hidden["devices"][0]["DeviceProperties"]["Port"], "80");
        assert_eq!(hidden["devices"][0]["DeviceProperties"]["Note"], REDACTED);
        let config: Value = serde_json::from_str(hidden["ConfigJson"].as_str().unwrap()).unwrap();
        assert_eq!(
            config["Modules"][0]["DeviceProperties"]["Password"],
            REDACTED
        );
        assert_eq!(config["Modules"][0]["DeviceProperties"]["User"], "u");

        let shown = redactor(true).redact(&doc);
        assert_eq!(shown["SessionId"], REDACTED, "session id is never shown");
        assert_eq!(shown["AccessToken"], "tok");
        assert_eq!(shown["devices"][0]["DeviceProperties"]["Password"], "p");
        assert_eq!(
            shown["devices"][0]["DeviceProperties"]["Note"], REDACTED,
            "known secret values stay hidden"
        );
    }
}
