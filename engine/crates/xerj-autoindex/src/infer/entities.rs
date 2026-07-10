//! Entity detection (semantic tags): email / url / ip / uuid.
//! A field is tagged when ≥90% of ≥20 non-null sampled values match.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entity {
    Email,
    Url,
    Ip,
    Uuid,
}

impl Entity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Entity::Email => "email",
            Entity::Url => "url",
            Entity::Ip => "ip",
            Entity::Uuid => "uuid",
        }
    }
}

struct Res {
    email: Regex,
    url: Regex,
    uuid: Regex,
}

fn res() -> &'static Res {
    static R: OnceLock<Res> = OnceLock::new();
    R.get_or_init(|| Res {
        email: Regex::new(r"^[^@\s]{1,64}@[^@\s]+\.[A-Za-z]{2,}$").unwrap(),
        url: Regex::new(r"^[a-z][a-z0-9+.-]*://\S+$").unwrap(),
        uuid: Regex::new(
            r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
        )
        .unwrap(),
    })
}

pub fn classify(s: &str) -> Option<Entity> {
    let t = s.trim();
    if t.is_empty() || t.len() > 512 {
        return None;
    }
    // IpAddr::parse is exact — version strings like "1.2.3" fail it.
    if t.parse::<std::net::IpAddr>().is_ok() {
        return Some(Entity::Ip);
    }
    let r = res();
    if r.uuid.is_match(t) {
        return Some(Entity::Uuid);
    }
    if r.email.is_match(t) {
        return Some(Entity::Email);
    }
    if r.url.is_match(t) {
        return Some(Entity::Url);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities() {
        assert_eq!(classify("a@b.example"), Some(Entity::Email));
        assert_eq!(classify("https://x.example/y"), Some(Entity::Url));
        assert_eq!(classify("192.231.71.123"), Some(Entity::Ip));
        assert_eq!(classify("::1"), Some(Entity::Ip));
        assert_eq!(classify("1.2.3"), None);
        assert_eq!(
            classify("741e7b6b-dbd2-4a7f-93a9-4ba50fb561d3"),
            Some(Entity::Uuid)
        );
        assert_eq!(classify("hello"), None);
    }
}
