//! The password salt. This is the only file that names the built-in value.
//!
//! `build.rs` writes the salt to `$OUT_DIR/salt.bin` (empty when none was available). To move the
//! salt into the source later, replace the `include_bytes!` line with a byte string literal.
use crate::config::Env;
use crate::errors::{CliError, Result};
use crate::output::hex;
use sha2::{Digest, Sha512};

pub static BUILT_IN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/salt.bin"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaltSource {
    Env,
    BuiltIn,
    Missing,
}

impl SaltSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SaltSource::Env => "env",
            SaltSource::BuiltIn => "built-in",
            SaltSource::Missing => "missing",
        }
    }
}

/// The salt in use. No Debug impl on purpose.
#[derive(Clone)]
pub struct Salt {
    bytes: Vec<u8>,
    pub source: SaltSource,
}

impl Salt {
    /// `TAPHOME_PASSWORD_SALT` overrides `built_in`. Pass [`BUILT_IN`] outside tests.
    pub fn resolve(env: &Env, built_in: &[u8]) -> Salt {
        if let Some(v) = env.get("TAPHOME_PASSWORD_SALT") {
            Salt {
                bytes: v.as_bytes().to_vec(),
                source: SaltSource::Env,
            }
        } else if !built_in.is_empty() {
            Salt {
                bytes: built_in.to_vec(),
                source: SaltSource::BuiltIn,
            }
        } else {
            Salt {
                bytes: Vec::new(),
                source: SaltSource::Missing,
            }
        }
    }

    /// Hash for the login message, or `salt_missing` when this binary has no salt.
    pub fn hash(&self, password: &str) -> Result<String> {
        if self.source == SaltSource::Missing {
            return Err(CliError::new(
                "salt_missing",
                "this binary was built without the password salt: use a release binary, or set TAPHOME_PASSWORD_SALT (see docs/auth.md)",
            ));
        }
        Ok(password_hash(password, &self.bytes))
    }

    /// Exact-value redaction needs the salt as text. Non-UTF-8 salts cannot appear in JSON output anyway.
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok().filter(|s| !s.is_empty())
    }
}

/// 2x SHA-512 over the UTF-8 password followed by the salt bytes, as 128 lower-case hex characters.
pub fn password_hash(password: &str, salt: &[u8]) -> String {
    let mut data = password.as_bytes().to_vec();
    data.extend_from_slice(salt);
    let first = Sha512::digest(&data);
    hex(&Sha512::digest(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Invented salt with a double quote and a backslash. Expected values computed independently:
    // with python3 hashlib: d = ("hunter2" + 'x"y\\z').encode(), then d = sha512(d).digest() twice, d.hex().
    const SALT: &[u8] = b"x\"y\\z";
    const HASH: &str =
        "b824d587eb1536d5edf2572ca6a4e2ed5515930b672e98689e658413cde813d930d29e84c18f8012b28c8e7acfb5db75ece39ab93cef96e4ce09e17b8ecbbf9b";

    #[test]
    fn hash_vector() {
        assert_eq!(password_hash("hunter2", SALT), HASH);
    }

    #[test]
    fn env_overrides_built_in_and_empty_is_missing() {
        let mut env = Env::default();
        assert_eq!(Salt::resolve(&env, b"built").source, SaltSource::BuiltIn);
        assert_eq!(Salt::resolve(&env, b"").source, SaltSource::Missing);
        env.set("TAPHOME_PASSWORD_SALT", "from-env");
        assert_eq!(Salt::resolve(&env, b"built").source, SaltSource::Env);
        assert_eq!(Salt::resolve(&env, b"").source, SaltSource::Env);
        assert_eq!(Salt::resolve(&Env::default(), b"").hash("p").unwrap_err().code, "salt_missing");
    }
}
