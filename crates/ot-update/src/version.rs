//! Versions: releases, and the builds between them.
//!
//! A release is a semantic version, `0.3.0` or `0.3.0-pre.1`, published from the
//! tag `v0.3.0`. A build is what `crates/ot-app/build.rs` says the running binary
//! is: a release (`0.2.1`), or a commit after one, git-describe style
//! (`0.2.1-19-g892159c`, `-dirty` for uncommitted changes, `0.2.1-g892159c` when the
//! count since the tag is unknown). A build after `0.2.1` is newer than `0.2.1` and
//! older than every release above it, so it is offered `0.2.2` but never `0.2.1`.

use std::cmp::Ordering;
use std::fmt;

/// A semantic version: `major.minor.patch`, optionally with a pre-release after a
/// hyphen. Ordered by semver precedence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated identifiers after the hyphen, empty for a release.
    pub pre: String,
}

/// A string that is not a version this crate understands.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a version: {0:?}")]
pub struct ParseError(pub String);

impl Version {
    /// Parse `1.2.3` or `1.2.3-pre.1`. Build metadata (`+…`) and a leading `v` are
    /// not accepted: the feed never produces them.
    ///
    /// # Errors
    /// [`ParseError`] if `s` is not of that form.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let bad = || ParseError(s.to_owned());
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, pre),
            None => (s, ""),
        };
        let mut nums = core.split('.').map(|n| {
            // Semver forbids leading zeros and signs; `u64::from_str` allows `+1`.
            let digits = !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
            let canonical = n == "0" || !n.starts_with('0');
            (digits && canonical)
                .then(|| n.parse::<u64>().ok())
                .flatten()
        });
        let (Some(Some(major)), Some(Some(minor)), Some(Some(patch)), None) =
            (nums.next(), nums.next(), nums.next(), nums.next())
        else {
            return Err(bad());
        };
        let pre_ok = pre.is_empty()
            || pre.split('.').all(|id| {
                !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if !pre_ok || s.ends_with('-') {
            return Err(bad());
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre: pre.to_owned(),
        })
    }

    /// No pre-release part.
    #[must_use]
    pub fn is_release(&self) -> bool {
        self.pre.is_empty()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| cmp_pre(&self.pre, &other.pre))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Semver pre-release precedence: none beats any; identifiers compare in turn,
/// numbers numerically and below words, words by ASCII; a longer list wins a tie.
fn cmp_pre(a: &str, b: &str) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    let mut xs = a.split('.');
    let mut ys = b.split('.');
    loop {
        let ord = match (xs.next(), ys.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => x.cmp(y),
            },
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
}

/// What the running binary is, as `build.rs` described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// The release it is, or the last one before it.
    pub base: Version,
    /// Commits since `base`, when known.
    pub commits: Option<u32>,
    /// Abbreviated commit, for anything but a clean release.
    pub hash: Option<String>,
    /// Built with uncommitted changes.
    pub dirty: bool,
    text: String,
}

impl Build {
    /// Parse a string made by `build.rs`.
    ///
    /// # Errors
    /// [`ParseError`] if it is not one of the shapes in the module docs.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let bad = || ParseError(s.to_owned());
        let (rest, dirty) = match s.strip_suffix("-dirty") {
            Some(rest) => (rest, true),
            None => (s, false),
        };
        let is_hash = |g: &str| {
            g.strip_prefix('g')
                .filter(|h| h.len() >= 7 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_owned)
        };
        // `0.2.1-19-g892159c`
        let mut parts = rest.rsplitn(3, '-');
        if let (Some(g), Some(n), Some(tag)) = (parts.next(), parts.next(), parts.next()) {
            if let (Some(hash), Ok(commits)) = (is_hash(g), n.parse::<u32>()) {
                return Ok(Self {
                    base: Version::parse(tag).map_err(|_| bad())?,
                    commits: Some(commits),
                    hash: Some(hash),
                    dirty,
                    text: s.to_owned(),
                });
            }
        }
        // `0.2.1-g892159c`
        if let Some((tag, g)) = rest.rsplit_once('-') {
            if let Some(hash) = is_hash(g) {
                return Ok(Self {
                    base: Version::parse(tag).map_err(|_| bad())?,
                    commits: None,
                    hash: Some(hash),
                    dirty,
                    text: s.to_owned(),
                });
            }
        }
        // `0.2.1`, `0.3.0-pre.1`. A dirty build always carries its hash.
        if dirty {
            return Err(bad());
        }
        Ok(Self {
            base: Version::parse(rest).map_err(|_| bad())?,
            commits: None,
            hash: None,
            dirty: false,
            text: s.to_owned(),
        })
    }

    /// A clean build of a release tag.
    #[must_use]
    pub fn is_release(&self) -> bool {
        self.hash.is_none() && !self.dirty
    }

    /// Whether `release` is newer than this build: above its base. A build after
    /// `0.2.1` is not offered `0.2.1` again.
    #[must_use]
    pub fn is_older_than(&self, release: &Version) -> bool {
        release > &self.base
    }

    /// The string it was parsed from, e.g. `0.2.1-19-g892159c-dirty`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for Build {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_parse_and_print() {
        assert_eq!(
            v("0.3.0"),
            Version {
                major: 0,
                minor: 3,
                patch: 0,
                pre: String::new()
            }
        );
        assert_eq!(v("1.20.300-rc.1").to_string(), "1.20.300-rc.1");
        assert!(v("1.0.0").is_release());
        assert!(!v("1.0.0-alpha").is_release());
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "01.2.3",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3+b",
            "+1.2.3",
            "1.2.x",
            "1.2.3-a_b",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn precedence_follows_semver() {
        // The example chain from semver.org, section 11.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("1.0.0").cmp(&v("1.0.0")), Ordering::Equal);
    }

    #[test]
    fn builds_parse_every_shape_build_rs_makes() {
        let b = Build::parse("0.2.1").unwrap();
        assert!(b.is_release());
        assert_eq!(b.base, v("0.2.1"));

        let b = Build::parse("0.2.1-19-g892159c").unwrap();
        assert_eq!(b.base, v("0.2.1"));
        assert_eq!(b.commits, Some(19));
        assert_eq!(b.hash.as_deref(), Some("892159c"));
        assert!(!b.dirty && !b.is_release());

        let b = Build::parse("0.2.1-0-g892159c-dirty").unwrap();
        assert_eq!((b.commits, b.dirty), (Some(0), true));

        let b = Build::parse("0.2.1-g892159c-dirty").unwrap();
        assert_eq!(
            (b.base.clone(), b.commits, b.dirty),
            (v("0.2.1"), None, true)
        );

        let b = Build::parse("0.3.0-pre.1").unwrap();
        assert!(b.is_release());
        assert_eq!(b.base, v("0.3.0-pre.1"));

        // A pre-release tag with a hyphen, and commits after it.
        let b = Build::parse("0.3.0-rc-1-4-gabcdef0").unwrap();
        assert_eq!((b.base.clone(), b.commits), (v("0.3.0-rc-1"), Some(4)));

        // Too short to be a hash: a pre-release word.
        let b = Build::parse("0.3.0-gabc").unwrap();
        assert_eq!(b.base, v("0.3.0-gabc"));
        assert!(b.is_release());

        assert_eq!(
            Build::parse("0.2.1-19-g892159c-dirty").unwrap().to_string(),
            "0.2.1-19-g892159c-dirty"
        );
        for bad in ["", "0.2.1-dirty", "v0.2.1", "g892159c", "0.2-19-g892159c"] {
            assert!(Build::parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn a_build_is_offered_only_releases_above_its_base() {
        let release = Build::parse("0.2.1").unwrap();
        let after = Build::parse("0.2.1-19-g892159c-dirty").unwrap();
        let pre = Build::parse("0.3.0-pre.1").unwrap();
        for b in [&release, &after] {
            assert!(!b.is_older_than(&v("0.2.0")));
            assert!(!b.is_older_than(&v("0.2.1")), "{b} is not older than 0.2.1");
            assert!(b.is_older_than(&v("0.2.2")));
            assert!(b.is_older_than(&v("0.3.0-pre.1")));
        }
        assert!(pre.is_older_than(&v("0.3.0")));
        assert!(!pre.is_older_than(&v("0.3.0-pre.1")));
        assert!(!pre.is_older_than(&v("0.2.9")));
    }
}
