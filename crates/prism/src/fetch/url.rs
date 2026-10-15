//! Just enough URL handling for `http(s)://host[:port]/path?query`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with `/`.
    pub target: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, String> {
        let (tls, rest) = if let Some(r) = s.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = s.strip_prefix("http://") {
            (false, r)
        } else {
            return Err("the URL must be http or https".into());
        };
        let rest = rest.split('#').next().unwrap_or(rest);
        let (auth, target) = match rest.find(['/', '?']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        if auth.contains('@') {
            return Err("credentials in the URL are not supported".into());
        }
        let default = if tls { 443 } else { 80 };
        let (host, port) = if let Some(v6) = auth.strip_prefix('[') {
            let (h, p) = v6.split_once(']').ok_or("bad IPv6 address in URL")?;
            (h.to_string(), parse_port(p.strip_prefix(':'), default)?)
        } else if let Some((h, p)) = auth.rsplit_once(':') {
            (h.to_string(), parse_port(Some(p), default)?)
        } else {
            (auth.to_string(), default)
        };
        if host.is_empty() || target.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err("invalid URL".into());
        }
        Ok(Url {
            tls,
            host,
            port,
            target,
        })
    }

    /// The `Host` header value.
    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == if self.tls { 443 } else { 80 } {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    /// Resolves a `Location` value against this URL.
    pub fn join(&self, loc: &str) -> Result<Url, String> {
        if loc.starts_with("http://") || loc.starts_with("https://") {
            return Url::parse(loc);
        }
        let scheme = if self.tls { "https" } else { "http" };
        let base = format!("{scheme}://{}", self.authority());
        if let Some(abs) = loc.strip_prefix("//") {
            return Url::parse(&format!("{scheme}://{abs}"));
        }
        if loc.starts_with('/') {
            return Url::parse(&format!("{base}{loc}"));
        }
        let dir = self.target.split('?').next().unwrap_or("/");
        let dir = &dir[..=dir.rfind('/').unwrap_or(0)];
        Url::parse(&format!("{base}{dir}{loc}"))
    }
}

fn parse_port(p: Option<&str>, default: u16) -> Result<u16, String> {
    match p {
        None | Some("") => Ok(default),
        Some(p) => p.parse().map_err(|_| "bad port in URL".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let u = Url::parse("https://example.com:8443/a/b.iso?x=1#f").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port),
            (true, "example.com", 8443)
        );
        assert_eq!(u.target, "/a/b.iso?x=1");
        assert_eq!(Url::parse("http://h?q").unwrap().target, "/?q");
        assert!(Url::parse("http://u@h/").is_err());
        assert!(Url::parse("ftp://h/").is_err());
    }

    #[test]
    fn joins() {
        let u = Url::parse("http://h/dir/file").unwrap();
        assert_eq!(u.join("other").unwrap().target, "/dir/other");
        assert_eq!(u.join("/root").unwrap().target, "/root");
        assert_eq!(u.join("https://x/y").unwrap().host, "x");
    }
}
