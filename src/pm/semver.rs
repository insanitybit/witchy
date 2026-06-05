//! A small semantic-version type and constraint matcher. Intentionally minimal:
//! `major.minor.patch`, with `^`, `~`, exact, and `>=` constraints — enough for
//! deterministic resolution without pulling in a full SemVer dependency.

use std::cmp::Ordering;
use std::fmt;

use super::{PmResult, err};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(s: &str) -> PmResult<Version> {
        let s = s.trim();
        let mut it = s.split('.');
        let mut next = |which: &str| -> PmResult<u64> {
            match it.next() {
                Some(p) => p
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| super::PmError(format!("bad version `{s}`: {which} not a number"))),
                None => Ok(0),
            }
        };
        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patch")?;
        if it.next().is_some() {
            return err(format!("bad version `{s}`: too many components"));
        }
        Ok(Version { major, minor, patch })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

/// A version constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Req {
    /// `^1.2.3` — compatible: same left-most non-zero component.
    Caret(Version),
    /// `~1.2.3` — same major.minor, patch may rise.
    Tilde(Version),
    /// `1.2.3` — exact.
    Exact(Version),
    /// `>=1.2.3`.
    AtLeast(Version),
    /// `*` — any version.
    Any,
}

impl Req {
    pub fn parse(s: &str) -> PmResult<Req> {
        let s = s.trim();
        if s == "*" {
            return Ok(Req::Any);
        }
        if let Some(rest) = s.strip_prefix('^') {
            return Ok(Req::Caret(Version::parse(rest)?));
        }
        if let Some(rest) = s.strip_prefix('~') {
            return Ok(Req::Tilde(Version::parse(rest)?));
        }
        if let Some(rest) = s.strip_prefix(">=") {
            return Ok(Req::AtLeast(Version::parse(rest)?));
        }
        Ok(Req::Exact(Version::parse(s)?))
    }

    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Req::Any => true,
            Req::Exact(r) => v == r,
            Req::AtLeast(r) => v >= r,
            Req::Tilde(r) => v.major == r.major && v.minor == r.minor && v.patch >= r.patch,
            Req::Caret(r) => {
                if v < r {
                    return false;
                }
                // Compatible range is bounded by the next left-most non-zero bump.
                if r.major > 0 {
                    v.major == r.major
                } else if r.minor > 0 {
                    v.major == 0 && v.minor == r.minor
                } else {
                    v.major == 0 && v.minor == 0 && v.patch == r.patch
                }
            }
        }
    }
}

impl fmt::Display for Req {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Req::Any => write!(f, "*"),
            Req::Exact(v) => write!(f, "{v}"),
            Req::AtLeast(v) => write!(f, ">={v}"),
            Req::Tilde(v) => write!(f, "~{v}"),
            Req::Caret(v) => write!(f, "^{v}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn ordering() {
        assert!(v("1.2.0") < v("1.10.0"));
        assert!(v("2.0.0") > v("1.9.9"));
        assert_eq!(v("1.2.3"), v("1.2.3"));
    }

    #[test]
    fn caret_major() {
        let r = Req::parse("^1.2.0").unwrap();
        assert!(r.matches(&v("1.2.0")));
        assert!(r.matches(&v("1.9.9")));
        assert!(!r.matches(&v("2.0.0")));
        assert!(!r.matches(&v("1.1.0")));
    }

    #[test]
    fn caret_zero_major() {
        // ^0.4.x is compatible only within 0.4.
        let r = Req::parse("^0.4.0").unwrap();
        assert!(r.matches(&v("0.4.5")));
        assert!(!r.matches(&v("0.5.0")));
    }

    #[test]
    fn exact_and_atleast_and_any() {
        assert!(Req::parse("1.2.3").unwrap().matches(&v("1.2.3")));
        assert!(!Req::parse("1.2.3").unwrap().matches(&v("1.2.4")));
        assert!(Req::parse(">=1.0.0").unwrap().matches(&v("3.0.0")));
        assert!(Req::parse("*").unwrap().matches(&v("9.9.9")));
    }
}
