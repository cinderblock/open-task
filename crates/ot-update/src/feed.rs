//! The release feed: signed checksums on the GitHub releases.
//!
//! Every release carries `SHA256SUMS` (one line per asset, as `sha256sum` writes
//! it) and `SHA256SUMS.minisig`, a minisign signature of it whose trusted comment
//! names the release: `open-task v0.3.0`. Finding out what the latest release is
//! takes no API and no JSON; everything that is trusted is signed:
//!
//! 1. Fetch the signature from `<releases>/latest/download/SHA256SUMS.minisig`.
//!    GitHub's `latest` skips drafts and pre-releases.
//! 2. Read the release it claims to be ([`claimed_version`]). Not verified yet, and
//!    used for one thing only: the URL of that release's `SHA256SUMS`.
//! 3. Fetch it and [`Feed::verify`]: the signature over the sums and over the
//!    comment, with the key compiled in. Now the version is trustworthy.
//! 4. Each asset is then checked against its line in the sums.

use minisign_verify::{PublicKey, Signature};

use crate::version::Version;

/// The checksums file of a release.
pub const SUMS: &str = "SHA256SUMS";
/// Its signature.
pub const SIGNATURE: &str = "SHA256SUMS.minisig";

/// open-task's releases. `OT_UPDATE_FEED` at build time replaces it, for testing
/// against a local server.
pub const RELEASES: &str = match option_env!("OT_UPDATE_FEED") {
    Some(url) => url,
    None => "https://github.com/cinderblock/open-task/releases",
};

/// The key releases are signed with: `minisign.pub` at the root of the repository.
/// `OT_UPDATE_PUBLIC_KEY` at build time replaces it, for testing with a throwaway
/// key; it only affects a binary built with it set.
pub const PUBLIC_KEY: &str = match option_env!("OT_UPDATE_PUBLIC_KEY") {
    Some(key) => key,
    None => include_str!("../../../minisign.pub"),
};

/// What the trusted comment says before the version.
const COMMENT_PREFIX: &str = "open-task v";

/// The Windows installer's asset name. Part of the naming contract in
/// `.github/workflows/release.yml`.
#[must_use]
pub fn installer_name(version: &Version) -> String {
    format!("open-task-v{version}-windows-setup.exe")
}

/// Why a release could not be trusted.
#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("the public key is malformed")]
    Key,
    #[error("the release's signature file is malformed")]
    SignatureFile,
    #[error("the release is signed with a different key")]
    WrongKey,
    #[error("the release's checksums do not match their signature")]
    BadSignature,
    #[error("the release's signed comment {0:?} does not name a version")]
    Comment(String),
    #[error("SHA256SUMS line {0} is malformed")]
    Sums(usize),
}

/// Where releases come from and the key they must carry.
#[derive(Debug, Clone)]
pub struct Feed {
    releases: String,
    key: PublicKey,
}

impl Feed {
    /// A feed at `releases` (no trailing slash) trusting `public_key`: a minisign
    /// public key file's text, or just its base64 line.
    ///
    /// # Errors
    /// [`FeedError::Key`] if the key does not decode.
    pub fn new(releases: &str, public_key: &str) -> Result<Self, FeedError> {
        let key = PublicKey::decode(public_key)
            .or_else(|_| PublicKey::from_base64(public_key.trim()))
            .map_err(|_| FeedError::Key)?;
        Ok(Self {
            releases: releases.trim_end_matches('/').to_owned(),
            key,
        })
    }

    /// open-task's own feed and key.
    ///
    /// # Panics
    /// If the compiled-in key is malformed, which a unit test rules out.
    #[must_use]
    pub fn official() -> Self {
        Self::new(RELEASES, PUBLIC_KEY).expect("the compiled-in public key decodes")
    }

    /// Where the latest release's signature is.
    #[must_use]
    pub fn latest_signature_url(&self) -> String {
        format!("{}/latest/download/{SIGNATURE}", self.releases)
    }

    /// Where an asset of a release is.
    #[must_use]
    pub fn asset_url(&self, version: &Version, asset: &str) -> String {
        format!("{}/download/v{version}/{asset}", self.releases)
    }

    /// The release's page, for people.
    #[must_use]
    pub fn release_page(&self, version: &Version) -> String {
        format!("{}/tag/v{version}", self.releases)
    }

    /// Check `sums` against `signature` and return what they describe.
    ///
    /// # Errors
    /// [`FeedError`] if the signature is not by this feed's key, does not cover
    /// these exact bytes, or the signed comment or the sums are not as expected.
    pub fn verify(&self, signature: &str, sums: &[u8]) -> Result<Release, FeedError> {
        let sig = Signature::decode(signature).map_err(|_| FeedError::SignatureFile)?;
        self.key.verify(sums, &sig, false).map_err(|e| match e {
            minisign_verify::Error::UnexpectedKeyId => FeedError::WrongKey,
            _ => FeedError::BadSignature,
        })?;
        // Verified above together with the signature, so this is authentic now.
        let version = comment_version(sig.trusted_comment())?;
        Ok(Release {
            version,
            assets: parse_sums(sums)?,
        })
    }
}

/// The release a signature says it is for, without checking anything. Only for
/// building the URL of its `SHA256SUMS`; [`Feed::verify`] decides.
///
/// # Errors
/// [`FeedError`] if the file or its comment does not parse.
pub fn claimed_version(signature: &str) -> Result<Version, FeedError> {
    let sig = Signature::decode(signature).map_err(|_| FeedError::SignatureFile)?;
    comment_version(sig.trusted_comment())
}

fn comment_version(comment: &str) -> Result<Version, FeedError> {
    comment
        .trim()
        .strip_prefix(COMMENT_PREFIX)
        .and_then(|v| Version::parse(v).ok())
        .ok_or_else(|| FeedError::Comment(comment.to_owned()))
}

/// A release whose checksums were signed with the feed's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub assets: Vec<Asset>,
}

/// One line of `SHA256SUMS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub sha256: [u8; 32],
}

impl Release {
    #[must_use]
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The Windows installer, if the release has one.
    #[must_use]
    pub fn installer(&self) -> Option<&Asset> {
        self.asset(&installer_name(&self.version))
    }
}

/// `sha256sum` output: 64 hex digits, a space, then a space (text mode) or `*`
/// (binary mode), then the name. Blank lines are allowed; nothing else is.
fn parse_sums(sums: &[u8]) -> Result<Vec<Asset>, FeedError> {
    let text = std::str::from_utf8(sums).map_err(|_| FeedError::Sums(1))?;
    let mut assets = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let bad = || FeedError::Sums(i + 1);
        if line.trim().is_empty() {
            continue;
        }
        let (hex, rest) = line.split_once(' ').ok_or_else(bad)?;
        let name = rest
            .strip_prefix(' ')
            .or_else(|| rest.strip_prefix('*'))
            .ok_or_else(bad)?;
        let sha256 = parse_hex(hex).ok_or_else(bad)?;
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(bad());
        }
        assets.push(Asset {
            name: name.to_owned(),
            sha256,
        });
    }
    Ok(assets)
}

fn parse_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    pub const TEST_KEY: &str = include_str!("../testdata/test.pub");
    pub const SUMS_FILE: &[u8] = include_bytes!("../testdata/SHA256SUMS");
    pub const SIG: &str = include_str!("../testdata/SHA256SUMS.minisig");
    pub const FAKE_SETUP: &[u8] = include_bytes!("../testdata/fake-installer.bin");

    pub fn test_feed() -> Feed {
        Feed::new("https://example.test/releases/", TEST_KEY).unwrap()
    }

    #[test]
    fn a_signed_release_verifies_and_lists_its_assets() {
        let r = test_feed().verify(SIG, SUMS_FILE).unwrap();
        assert_eq!(r.version, Version::parse("0.3.0").unwrap());
        assert_eq!(r.assets.len(), 3);
        let setup = r.installer().expect("the installer is listed");
        assert_eq!(setup.name, "open-task-v0.3.0-windows-setup.exe");
        let digest: [u8; 32] = Sha256::digest(FAKE_SETUP).into();
        assert_eq!(setup.sha256, digest);
        assert!(r
            .asset("open-task-v0.3.0-x86_64-pc-windows-msvc.zip")
            .is_some());
        assert_eq!(claimed_version(SIG).unwrap(), r.version);
    }

    #[test]
    fn line_endings_do_not_matter_for_the_signature_file_or_the_key() {
        let lf_sig = SIG.replace("\r\n", "\n");
        let lf_key = TEST_KEY.replace("\r\n", "\n");
        let feed = Feed::new("x", &lf_key).unwrap();
        assert!(feed.verify(&lf_sig, SUMS_FILE).is_ok());
        // The bare base64 line works as a key too.
        let b64 = TEST_KEY.lines().nth(1).unwrap();
        assert!(Feed::new("x", b64).unwrap().verify(SIG, SUMS_FILE).is_ok());
    }

    #[test]
    fn tampering_the_wrong_key_or_comment_is_refused() {
        let feed = test_feed();
        let mut tampered = SUMS_FILE.to_vec();
        tampered[0] ^= 1;
        assert!(matches!(
            feed.verify(SIG, &tampered),
            Err(FeedError::BadSignature)
        ));
        let other = include_str!("../testdata/other-key.minisig");
        assert!(matches!(
            feed.verify(other, SUMS_FILE),
            Err(FeedError::WrongKey)
        ));
        // Signed properly, but the comment is not "open-task v<version>".
        let wrong = include_str!("../testdata/wrong-comment.minisig");
        assert!(matches!(
            feed.verify(wrong, SUMS_FILE),
            Err(FeedError::Comment(c)) if c == "open-task 0.3.0"
        ));
        // The trusted comment is signed too: editing it breaks the signature.
        let edited = SIG.replace("open-task v0.3.0", "open-task v9.3.0");
        assert_eq!(claimed_version(&edited).unwrap().major, 9);
        assert!(matches!(
            feed.verify(&edited, SUMS_FILE),
            Err(FeedError::BadSignature)
        ));
        assert!(matches!(
            feed.verify("not a signature", SUMS_FILE),
            Err(FeedError::SignatureFile)
        ));
        assert!(matches!(Feed::new("x", "nope"), Err(FeedError::Key)));
    }

    #[test]
    fn sums_parse_as_sha256sum_writes_them() {
        let hash = "ab".repeat(32);
        let text = format!("{hash}  a.zip\n{hash} *b.exe\r\n\n");
        let assets = parse_sums(text.as_bytes()).unwrap();
        assert_eq!(
            assets.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["a.zip", "b.exe"]
        );
        assert_eq!(assets[0].sha256, [0xAB; 32]);
        for bad in [
            format!("{hash} a.zip"),
            format!("{}  a.zip", &hash[1..]),
            format!("{}  a.zip", "zz".repeat(32)),
            format!("{hash}  dir/a.zip"),
            format!("{hash}  "),
        ] {
            assert!(parse_sums(bad.as_bytes()).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn urls_follow_the_github_layout() {
        let feed = test_feed();
        let v = Version::parse("0.3.0").unwrap();
        assert_eq!(
            feed.latest_signature_url(),
            "https://example.test/releases/latest/download/SHA256SUMS.minisig"
        );
        assert_eq!(
            feed.asset_url(&v, &installer_name(&v)),
            "https://example.test/releases/download/v0.3.0/open-task-v0.3.0-windows-setup.exe"
        );
        assert_eq!(
            feed.release_page(&v),
            "https://example.test/releases/tag/v0.3.0"
        );
    }

    #[test]
    fn the_official_key_decodes() {
        let _ = Feed::official();
    }
}
