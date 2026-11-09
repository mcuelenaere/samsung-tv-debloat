//! Package inspection: zip hygiene, manifest metadata, identity/privilege
//! policy, and a best-effort signature label for the approval prompt.
//!
//! This module never trusts the manifest for authorization. It only produces
//! the metadata the user sees and rejects packages that are clearly outside the
//! public/partner privilege ceiling; the stock installer remains the authority.

use crate::protocol::{
    prompt_text, InstallMeta, PackageType, SignatureStatus, MAX_PKGID_BYTES, MAX_PKG_NAME_BYTES,
    MAX_PKG_VERSION_BYTES, MAX_PRIVILEGES, MAX_PRIVILEGE_BYTES,
};
use base64::Engine as _;
use sha1::{Digest, Sha1};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

pub const MAX_ZIP_ENTRIES: usize = 4096;
pub const MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 512 * 1024;

const TPK_MANIFEST: &str = "tizen-manifest.xml";
const WGT_MANIFEST: &str = "config.xml";
const AUTHOR_SIGNATURE: &str = "author-signature.xml";

/// Package-id prefixes reserved for the platform. Shadowing them is refused
/// regardless of the signer.
const FORBIDDEN_PKGID_PREFIXES: &[&str] = &["com.samsung.", "org.tizen.", "samsung."];

/// Privileges that are platform-only in Tizen's privilege table, or that only
/// Samsung distributes. Enrolling the user distributor CA in `tizen-partner`
/// (§4) already allows public and partner privileges; refusing platform-only
/// ones up front makes the failure understandable. This list is deliberately
/// conservative and is *not* the security boundary.
const PLATFORM_ONLY_PRIVILEGES: &[&str] = &[
    "http://tizen.org/privilege/account.read",
    "http://tizen.org/privilege/account.write",
    "http://tizen.org/privilege/appmanager.admin",
    "http://tizen.org/privilege/audit",
    "http://tizen.org/privilege/certificate",
    "http://tizen.org/privilege/cloud.admin",
    "http://tizen.org/privilege/filesystem.internal",
    "http://tizen.org/privilege/keymanager.admin",
    "http://tizen.org/privilege/log",
    "http://tizen.org/privilege/packagemanager.admin",
    "http://tizen.org/privilege/reboot",
    "http://tizen.org/privilege/recents",
    "http://tizen.org/privilege/systemmanager",
    "http://tizen.org/privilege/telephony.admin",
    "http://tizen.org/privilege/zone",
];

const SAMSUNG_PRIVILEGE_PREFIX: &str = "http://developer.samsung.com/tizen/privilege/";

#[derive(Debug)]
pub enum MetaError {
    BadPackage(&'static str),
    Forbidden(String),
}

impl std::fmt::Display for MetaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadPackage(reason) => write!(formatter, "invalid package: {reason}"),
            Self::Forbidden(reason) => write!(formatter, "package refused: {reason}"),
        }
    }
}

/// Fingerprints the daemon knows about. Used only to label the signature on the
/// prompt; the stock installer performs the real chain validation.
#[derive(Debug, Clone, Default)]
pub struct SignatureContext {
    /// Distributor root CA (public/partner domains).
    pub user_ca_sha1: Option<[u8; 20]>,
    /// Author root CA (`tizen-developer` domain). A package signed on-device
    /// carries both chains, so either root identifies it as ours.
    pub author_ca_sha1: Option<[u8; 20]>,
    pub known_fingerprints: Vec<[u8; 20]>,
}

#[derive(Debug, Clone)]
pub struct PackageMeta {
    pub pkgid: String,
    pub name: String,
    pub pkg_version: String,
    pub pkg_type: PackageType,
    pub privileges: Vec<String>,
    pub signature: SignatureStatus,
}

impl PackageMeta {
    pub fn into_install_meta(self, size_bytes: u64, is_upgrade: bool) -> InstallMeta {
        InstallMeta {
            pkgid: self.pkgid,
            name: self.name,
            pkg_version: self.pkg_version,
            pkg_type: self.pkg_type,
            size_bytes,
            privileges: self.privileges,
            signature: self.signature,
            is_upgrade,
        }
    }
}

/// Inspect a staged package.
pub fn inspect(
    path: &Path,
    pkg_type: PackageType,
    signatures: &SignatureContext,
) -> Result<PackageMeta, MetaError> {
    let file = File::open(path).map_err(|_| MetaError::BadPackage("cannot open package"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|_| MetaError::BadPackage("not a zip archive"))?;

    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(MetaError::BadPackage("too many zip entries"));
    }
    let mut uncompressed = 0_u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|_| MetaError::BadPackage("unreadable zip entry"))?;
        let name = entry.name().to_owned();
        if !is_safe_entry_name(&name) {
            return Err(MetaError::BadPackage("unsafe zip entry name"));
        }
        uncompressed = uncompressed.saturating_add(entry.size());
        if uncompressed > MAX_UNCOMPRESSED_BYTES {
            return Err(MetaError::BadPackage("zip expands beyond the size limit"));
        }
    }

    let manifest_name = match pkg_type {
        PackageType::Tpk => TPK_MANIFEST,
        PackageType::Wgt => WGT_MANIFEST,
    };
    let manifest = read_entry(&mut archive, manifest_name, MAX_MANIFEST_BYTES)?
        .ok_or(MetaError::BadPackage("manifest is missing"))?;
    let manifest = String::from_utf8(manifest)
        .map_err(|_| MetaError::BadPackage("manifest is not valid UTF-8"))?;

    let (pkgid, name, version, privileges) = parse_manifest(&manifest, pkg_type)?;
    if !crate::identity::is_valid_package_id(&pkgid) {
        return Err(MetaError::BadPackage("manifest package id is invalid"));
    }
    if let Some(prefix) = FORBIDDEN_PKGID_PREFIXES
        .iter()
        .find(|prefix| pkgid.starts_with(**prefix))
    {
        return Err(MetaError::Forbidden(format!(
            "package id uses the reserved prefix {prefix}"
        )));
    }
    let rejected: Vec<&str> = privileges
        .iter()
        .filter(|privilege| is_platform_only(privilege))
        .map(String::as_str)
        .collect();
    if !rejected.is_empty() {
        return Err(MetaError::Forbidden(format!(
            "privileges require a partner/platform certificate: {}",
            rejected.join(", ")
        )));
    }

    let (has_author, has_distributor) = signature_entries(&mut archive);
    let chain = if has_author {
        signature_chain(&mut archive, AUTHOR_SIGNATURE)?
    } else if has_distributor {
        signature_chain(&mut archive, "signature1.xml")?
    } else {
        Vec::new()
    };
    let signature = classify_signature(has_author, has_distributor, &chain, signatures);

    Ok(PackageMeta {
        pkgid: prompt_text(&pkgid, MAX_PKGID_BYTES),
        name: prompt_text(&name, MAX_PKG_NAME_BYTES),
        pkg_version: prompt_text(&version, MAX_PKG_VERSION_BYTES),
        pkg_type,
        privileges: privileges
            .iter()
            .take(MAX_PRIVILEGES)
            .map(|privilege| prompt_text(privilege, MAX_PRIVILEGE_BYTES))
            .collect(),
        signature,
    })
}

/// `true` if `pkgid` belongs to the platform namespace.
pub fn is_reserved_pkgid(pkgid: &str) -> bool {
    FORBIDDEN_PKGID_PREFIXES
        .iter()
        .any(|prefix| pkgid.starts_with(prefix))
}

pub(crate) fn is_safe_entry_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') {
        return false;
    }
    if name.contains('\0') || name.contains('\\') {
        return false;
    }
    // Windows-style drive prefix, just in case.
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return false;
    }
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    !trimmed.is_empty()
        && trimmed
            .split('/')
            .all(|component| !component.is_empty() && component != "..")
}

fn read_entry(
    archive: &mut ZipArchive<File>,
    name: &str,
    maximum: u64,
) -> Result<Option<Vec<u8>>, MetaError> {
    let mut entry = match archive.by_name(name) {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(_) => return Err(MetaError::BadPackage("cannot read manifest")),
    };
    if entry.size() > maximum {
        return Err(MetaError::BadPackage("manifest is too large"));
    }
    // The central directory can lie about the uncompressed size; cap the read
    // itself so a crafted archive cannot balloon memory.
    let mut bytes = Vec::with_capacity(entry.size().min(maximum) as usize);
    (&mut entry)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MetaError::BadPackage("cannot read manifest"))?;
    if bytes.len() as u64 > maximum {
        return Err(MetaError::BadPackage("manifest is too large"));
    }
    Ok(Some(bytes))
}

fn parse_manifest(
    text: &str,
    pkg_type: PackageType,
) -> Result<(String, String, String, Vec<String>), MetaError> {
    let document = roxmltree::Document::parse(text)
        .map_err(|_| MetaError::BadPackage("manifest is not XML"))?;
    let root = document.root_element();
    let expected = match pkg_type {
        PackageType::Tpk => "manifest",
        PackageType::Wgt => "widget",
    };
    if root.tag_name().name() != expected {
        return Err(MetaError::BadPackage("unexpected manifest root element"));
    }

    let mut pkgid = match pkg_type {
        PackageType::Tpk => attribute(&root, "package"),
        PackageType::Wgt => attribute(&root, "id"),
    };
    let mut version = attribute(&root, "version");
    let mut name = String::new();
    let mut application_package = String::new();

    for node in root.descendants().filter(|node| node.is_element()) {
        match node.tag_name().name() {
            // tpk: <label>; wgt: <name>
            "label" | "name" if name.is_empty() => {
                name = node.text().unwrap_or("").trim().to_owned();
            }
            // A wgt's real package id lives on <tizen:application package="...">;
            // the widget id is usually a URL.
            "application" if pkg_type == PackageType::Wgt => {
                application_package = attribute(&node, "package");
                if version.is_empty() {
                    version = attribute(&node, "version");
                }
            }
            _ => {}
        }
    }
    if pkg_type == PackageType::Wgt && !application_package.is_empty() {
        pkgid = application_package;
    }

    if pkgid.is_empty() || version.is_empty() {
        return Err(MetaError::BadPackage(
            "manifest lacks package id or version",
        ));
    }

    let privileges = root
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "privilege")
        .filter_map(|node| {
            // wgt uses <tizen:privilege name="..."/>; tpk uses <privilege>URI</privilege>.
            let value = node
                .attribute("name")
                .map(str::to_owned)
                .or_else(|| node.text().map(str::to_owned))?;
            Some(value.trim().to_owned())
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if privileges.len() > MAX_PRIVILEGES {
        return Err(MetaError::BadPackage(
            "manifest requests too many privileges",
        ));
    }

    Ok((pkgid, name, version, privileges))
}

fn attribute(node: &roxmltree::Node<'_, '_>, name: &str) -> String {
    node.attribute(name).unwrap_or("").trim().to_owned()
}

fn is_platform_only(privilege: &str) -> bool {
    PLATFORM_ONLY_PRIVILEGES.contains(&privilege) || privilege.starts_with(SAMSUNG_PRIVILEGE_PREFIX)
}

fn is_signature_entry(name: &str) -> bool {
    if name == AUTHOR_SIGNATURE {
        return true;
    }
    name.strip_prefix("signature")
        .and_then(|rest| rest.strip_suffix(".xml"))
        .is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn signature_entries(archive: &mut ZipArchive<File>) -> (bool, bool) {
    let mut author = false;
    let mut distributor = false;
    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index_raw(index) else {
            continue;
        };
        let name = entry.name().to_owned();
        if name == AUTHOR_SIGNATURE {
            author = true;
        } else if is_signature_entry(&name) {
            distributor = true;
        }
    }
    (author, distributor)
}

/// SHA-1 fingerprints of every certificate in the signature file's
/// `<X509Data>`. This is a label, not verification: the bytes are not checked.
fn signature_chain(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<[u8; 20]>, MetaError> {
    let Some(bytes) = read_entry(archive, name, MAX_SIGNATURE_BYTES)? else {
        return Ok(Vec::new());
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut chain = Vec::new();
    for node in xml_elements(&text, "X509Certificate") {
        let compact: String = node
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let Ok(der) = base64::engine::general_purpose::STANDARD.decode(compact.as_bytes()) else {
            continue;
        };
        chain.push(sha1_digest(&der));
    }
    Ok(chain)
}

/// Extremely small, non-validating element-text extractor. Signature files are
/// generated by Tizen Studio (or our signer); we only need the certificate
/// blobs and a malformed file must not panic the daemon.
fn xml_elements<'a>(text: &'a str, local_name: &str) -> Vec<&'a str> {
    let mut values = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let Some(offset) = rest[start..].find('>') else {
            break;
        };
        let tag_end = start + offset + 1;
        let tag = &rest[start + 1..tag_end - 1];
        let name = tag.split_ascii_whitespace().next().unwrap_or("");
        let closing = name.starts_with('/');
        let name = name.trim_start_matches('/');
        if !closing && !name.is_empty() && !tag.ends_with('/') {
            let close = if let Some((prefix, local)) = name.split_once(':') {
                (local == local_name).then(|| format!("</{prefix}:{local_name}>"))
            } else if name == local_name {
                Some(format!("</{local_name}>"))
            } else {
                None
            };
            if let Some(close) = close {
                if let Some(end) = rest[tag_end..].find(&close) {
                    values.push(&rest[tag_end..tag_end + end]);
                    rest = &rest[tag_end + end..];
                    continue;
                }
            }
        }
        rest = &rest[tag_end..];
    }
    values
}

pub fn sha1_digest(bytes: &[u8]) -> [u8; 20] {
    let digest = Sha1::digest(bytes);
    let mut output = [0_u8; 20];
    output.copy_from_slice(&digest);
    output
}

pub fn classify_signature(
    has_author: bool,
    has_distributor: bool,
    chain: &[[u8; 20]],
    context: &SignatureContext,
) -> SignatureStatus {
    if !has_author && !has_distributor {
        return SignatureStatus::UnsignedWillResign;
    }
    for ours in [context.user_ca_sha1, context.author_ca_sha1]
        .into_iter()
        .flatten()
    {
        if chain.contains(&ours) {
            return SignatureStatus::UserCa;
        }
    }
    if chain
        .iter()
        .any(|fingerprint| context.known_fingerprints.contains(fingerprint))
    {
        return SignatureStatus::Samsung;
    }
    SignatureStatus::ForeignWillResign
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use zip::write::SimpleFileOptions;

    struct Fixture {
        _directory: tempfile::TempDir,
        path: PathBuf,
    }

    fn package(entries: &[(&str, &[u8])]) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("package.wgt");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, body) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
        Fixture {
            _directory: directory,
            path,
        }
    }

    fn wgt_config(privileges: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<widget xmlns="http://www.w3.org/ns/widgets"
        xmlns:tizen="http://tizen.org/ns/widgets"
        id="http://example.com/Example" version="1.0.0">
  <name>Example</name>
  <tizen:application id="org.example.app" package="org.example.app"/>
  {privileges}
</widget>"#
        )
    }

    #[test]
    fn parses_wgt_metadata_and_privileges() {
        let fixture = package(&[(
            "config.xml",
            wgt_config(
                "<tizen:privilege name=\"http://tizen.org/privilege/internet\"/>\
                 <tizen:privilege name=\"http://tizen.org/privilege/filesystem.read\"/>",
            )
            .as_bytes(),
        )]);
        let meta = inspect(
            &fixture.path,
            PackageType::Wgt,
            &SignatureContext::default(),
        )
        .unwrap();
        assert_eq!(meta.pkgid, "org.example.app");
        assert_eq!(meta.name, "Example");
        assert_eq!(meta.pkg_version, "1.0.0");
        assert_eq!(
            meta.privileges,
            vec![
                "http://tizen.org/privilege/internet".to_owned(),
                "http://tizen.org/privilege/filesystem.read".to_owned()
            ]
        );
        assert_eq!(meta.signature, SignatureStatus::UnsignedWillResign);
    }

    #[test]
    fn accepts_privileges_as_element_text_too() {
        let fixture = package(&[(
            "config.xml",
            wgt_config("<tizen:privilege>http://tizen.org/privilege/internet</tizen:privilege>")
                .as_bytes(),
        )]);
        let meta = inspect(
            &fixture.path,
            PackageType::Wgt,
            &SignatureContext::default(),
        )
        .unwrap();
        assert_eq!(
            meta.privileges,
            vec!["http://tizen.org/privilege/internet".to_owned()]
        );
    }

    #[test]
    fn parses_tpk_manifest() {
        let fixture = package(&[(
            "tizen-manifest.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest xmlns="http://tizen.org/ns/packages" package="org.example.tpk" version="2.0.0" api-version="5.5">
  <label>TPK Example</label>
  <privileges>
    <privilege>http://tizen.org/privilege/internet</privilege>
  </privileges>
</manifest>"#,
        )]);
        let meta = inspect(
            &fixture.path,
            PackageType::Tpk,
            &SignatureContext::default(),
        )
        .unwrap();
        assert_eq!(meta.pkgid, "org.example.tpk");
        assert_eq!(meta.name, "TPK Example");
        assert_eq!(meta.pkg_version, "2.0.0");
        assert_eq!(meta.pkg_type, PackageType::Tpk);
    }

    #[test]
    fn rejects_unsafe_zip_names() {
        for name in ["../evil", "/absolute", "a/../../b", "a\\b"] {
            let fixture = package(&[("config.xml", wgt_config("").as_bytes()), (name, b"x")]);
            assert!(
                matches!(
                    inspect(
                        &fixture.path,
                        PackageType::Wgt,
                        &SignatureContext::default()
                    ),
                    Err(MetaError::BadPackage(_))
                ),
                "{name} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_reserved_ids_and_platform_privileges() {
        let fixture = package(&[(
            "config.xml",
            wgt_config("")
                .replace(
                    "package=\"org.example.app\"",
                    "package=\"com.samsung.evil\"",
                )
                .as_bytes(),
        )]);
        assert!(matches!(
            inspect(
                &fixture.path,
                PackageType::Wgt,
                &SignatureContext::default()
            ),
            Err(MetaError::Forbidden(_))
        ));

        let fixture = package(&[(
            "config.xml",
            wgt_config(
                "<tizen:privilege>http://tizen.org/privilege/appmanager.admin</tizen:privilege>",
            )
            .as_bytes(),
        )]);
        match inspect(
            &fixture.path,
            PackageType::Wgt,
            &SignatureContext::default(),
        ) {
            Err(MetaError::Forbidden(detail)) => assert!(detail.contains("appmanager.admin")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn missing_manifest_is_bad_package() {
        let fixture = package(&[("other.xml", b"<x/>")]);
        assert!(matches!(
            inspect(
                &fixture.path,
                PackageType::Wgt,
                &SignatureContext::default()
            ),
            Err(MetaError::BadPackage("manifest is missing"))
        ));
    }

    #[test]
    fn classifies_signature_roots() {
        let ours = [1_u8; 20];
        let samsung = [2_u8; 20];
        let author = [3_u8; 20];
        let context = SignatureContext {
            user_ca_sha1: Some(ours),
            author_ca_sha1: Some(author),
            known_fingerprints: vec![ours, author, samsung],
        };
        assert_eq!(
            classify_signature(false, false, &[], &context),
            SignatureStatus::UnsignedWillResign
        );
        assert_eq!(
            classify_signature(true, true, &[[9_u8; 20], ours], &context),
            SignatureStatus::UserCa
        );
        assert_eq!(
            classify_signature(true, true, &[[9_u8; 20], author], &context),
            SignatureStatus::UserCa
        );
        assert_eq!(
            classify_signature(true, true, &[[9_u8; 20], samsung], &context),
            SignatureStatus::Samsung
        );
        assert_eq!(
            classify_signature(true, true, &[[4_u8; 20]], &context),
            SignatureStatus::ForeignWillResign
        );
        assert_eq!(
            classify_signature(true, false, &[], &context),
            SignatureStatus::ForeignWillResign
        );
    }

    #[test]
    fn extracts_certificates_from_signature_xml() {
        let xml = r#"<Signature xmlns="http://www.w3.org/2000/09/xmldsig#">
          <KeyInfo>
            <X509Data>
              <X509Certificate>QUJD</X509Certificate>
            </X509Data>
          </KeyInfo>
        </Signature>"#;
        let values = xml_elements(xml, "X509Certificate");
        assert_eq!(values, vec!["QUJD"]);

        let prefixed = r#"<ds:Signature><ds:KeyInfo><ds:X509Data>
              <ds:X509Certificate>REVG</ds:X509Certificate>
        </ds:X509Data></ds:KeyInfo></ds:Signature>"#;
        assert_eq!(xml_elements(prefixed, "X509Certificate"), vec!["REVG"]);

        let der = base64::engine::general_purpose::STANDARD
            .decode(values[0].as_bytes())
            .unwrap();
        assert_eq!(der, b"ABC");
        assert_eq!(sha1_digest(b"ABC").len(), 20);
    }

    #[test]
    fn entry_names_are_checked_after_normalization() {
        assert!(is_safe_entry_name("config.xml"));
        assert!(is_safe_entry_name("res/icon.png"));
        assert!(is_safe_entry_name("res/"));
        assert!(!is_safe_entry_name("../x"));
        assert!(!is_safe_entry_name("a/../x"));
        assert!(!is_safe_entry_name("/x"));
        assert!(!is_safe_entry_name(""));
        assert!(!is_safe_entry_name("a\\b"));
    }
}
