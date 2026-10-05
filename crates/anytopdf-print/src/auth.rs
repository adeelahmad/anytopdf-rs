//! Print users: Argon2id password hashes in a JSON file and HTTP Basic
//! credential checks for IPP requests.

use anyhow::{Context, Result, bail};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use base64ct::{Base64, Encoding};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const USERS_SCHEMA: &str = "anytopdf.print-users/1";

/// Verified against for unknown user names; matches no password.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$YW55dG9wZGYtZHVtbXk$KY7mXyXdTzvRL0vdsPEhaS8IjA6JhyEFvt6SoS5/Lb8";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Users {
    schema_version: String,
    users: BTreeMap<String, String>,
}

impl Users {
    pub fn new() -> Self {
        Users {
            schema_version: USERS_SCHEMA.into(),
            users: BTreeMap::new(),
        }
    }

    /// Reads a users file; a missing file is an empty user list.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Users::new());
        }
        Users::load(path)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read users file {}", path.display()))?;
        let users: Users = serde_json::from_str(&text)
            .with_context(|| format!("users file {} is not valid JSON", path.display()))?;
        if users.schema_version != USERS_SCHEMA {
            bail!(
                "users file {} has schema_version {:?}, expected {USERS_SCHEMA:?}",
                path.display(),
                users.schema_version
            );
        }
        for (name, hash) in &users.users {
            PasswordHash::new(hash).map_err(|e| {
                anyhow::anyhow!("users file {}: bad hash for {name:?}: {e}", path.display())
            })?;
        }
        Ok(users)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(path)
            .with_context(|| format!("cannot write users file {}", path.display()))?;
        std::io::Write::write_all(&mut file, text.as_bytes())
            .with_context(|| format!("cannot write users file {}", path.display()))
    }

    pub fn set_password(&mut self, name: &str, password: &str) -> Result<()> {
        validate_name(name)?;
        if password.is_empty() {
            bail!("password must not be empty");
        }
        self.users
            .insert(name.to_string(), hash_password(password)?);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.users.remove(name).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.users.keys().map(String::as_str)
    }

    /// Checks a user name and password; unknown users still pay for one hash
    /// so timing does not reveal which names exist.
    pub fn verify(&self, name: &str, password: &str) -> bool {
        let (known, hash) = match self.users.get(name) {
            Some(hash) => (true, hash.as_str()),
            None => (false, DUMMY_HASH),
        };
        let ok = PasswordHash::new(hash)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false);
        known && ok
    }

    /// Checks an HTTP `Authorization` header value and returns the user name.
    pub fn authorize(&self, header: Option<&str>) -> Option<String> {
        let (name, password) = parse_basic(header?)?;
        self.verify(&name, &password).then_some(name)
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || name.contains(':')
        || name.chars().any(char::is_control)
    {
        bail!("user name must be 1-255 bytes with no ':' or control characters");
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String> {
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..16]);
    let salt = SaltString::encode_b64(&salt).map_err(|e| anyhow::anyhow!("salt: {e}"))?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("cannot hash password: {e}"))?
        .to_string())
}

/// Parses `Basic <base64(user:password)>`.
pub fn parse_basic(header: &str) -> Option<(String, String)> {
    let (scheme, value) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = Base64::decode_vec(value.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, password) = text.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic(user: &str, password: &str) -> String {
        format!(
            "Basic {}",
            Base64::encode_string(format!("{user}:{password}").as_bytes())
        )
    }

    #[test]
    fn correct_password_authorizes_and_wrong_one_does_not() {
        let mut users = Users::new();
        users.set_password("adeel", "s3cret:with-colon").unwrap();
        assert_eq!(
            users.authorize(Some(&basic("adeel", "s3cret:with-colon"))),
            Some("adeel".into())
        );
        assert_eq!(users.authorize(Some(&basic("adeel", "wrong"))), None);
        assert_eq!(
            users.authorize(Some(&basic("other", "s3cret:with-colon"))),
            None
        );
        assert_eq!(users.authorize(None), None);
        assert_eq!(users.authorize(Some("Bearer abc")), None);
    }

    #[test]
    fn users_file_round_trips_without_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users.json");
        let mut users = Users::new();
        users.set_password("adeel", "hunter2").unwrap();
        users.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(USERS_SCHEMA));
        assert!(!text.contains("hunter2"));
        assert!(Users::load(&path).unwrap().verify("adeel", "hunter2"));
    }

    #[test]
    fn users_file_with_wrong_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users.json");
        std::fs::write(&path, r#"{"schema_version":"x/9","users":{}}"#).unwrap();
        assert!(Users::load(&path).is_err());
    }

    #[test]
    fn unknown_user_dummy_hash_is_well_formed() {
        let users = Users::new();
        assert!(!users.verify("nobody", "pw"));
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
    }

    #[test]
    fn user_names_with_colons_are_rejected() {
        assert!(Users::new().set_password("a:b", "pw").is_err());
        assert!(Users::new().set_password("a", "").is_err());
    }

    #[test]
    fn basic_scheme_is_case_insensitive() {
        let header = basic("u", "p").replace("Basic", "bAsIc");
        assert_eq!(parse_basic(&header), Some(("u".into(), "p".into())));
    }
}
