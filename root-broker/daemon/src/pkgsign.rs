//! On-device re-signing of packages with the user CAs (M3).
//!
//! Tizen validates two independent XML-DSig chains in a package:
//!
//! - `author-signature.xml`: every payload file, signed by an author
//!   certificate whose root is in the `tizen-developer` fingerprint domain;
//! - `signature1.xml`: `author-signature.xml` plus every payload file, signed by
//!   a distributor certificate whose root is in `tizen-public`/`tizen-partner`
//!   (and *not* in `tizen-developer`).
//!
//! Both use exclusive C14N for `SignedInfo`, RSA-SHA512, a digest per file, and
//! a `#prop` signature-properties object (`role-author` / `role-distributor`).
//! The firmware ships `xmlsec1` (linked against libxmlsec1/libxml2), which
//! computes the digests, the canonical form, and the signature value; this
//! module builds the templates, drives the tool, and rewrites the archive.

use crate::executor;
use crate::pkgmeta::{is_safe_entry_name, sha1_digest};
use crate::protocol::PackageType;
use crate::trust::{certificate_der, TrustPaths};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// Each `xmlsec1` run is local and small; it must never stall the daemon.
pub const SIGN_TIMEOUT_MS: u64 = 30_000;
/// Caps mirror the staging limits.
const MAX_SIGN_FILES: usize = 4096;
const MAX_SIGN_BYTES: u64 = 512 * 1024 * 1024;
const XML_DECLARATION: &str = "<?xml";

#[derive(Debug, Clone)]
pub struct SigningKeys {
    pub xmlsec1: PathBuf,
    pub author_key: PathBuf,
    pub author_cert: PathBuf,
    pub author_ca: PathBuf,
    pub distributor_key: PathBuf,
    pub distributor_cert: PathBuf,
    pub distributor_ca: PathBuf,
}

impl SigningKeys {
    pub fn from_trust(paths: &TrustPaths, xmlsec1: PathBuf) -> Self {
        Self {
            xmlsec1,
            author_key: paths.author_key_file(),
            author_cert: paths.author_cert_file(),
            author_ca: paths.author_ca_file(),
            distributor_key: paths.distributor_key_file(),
            distributor_cert: paths.distributor_cert_file(),
            distributor_ca: paths.ca_file(),
        }
    }

    /// SHA-1 of the author end-entity certificate. Tizen rejects upgrades
    /// signed by a different author, so the daemon compares this with the
    /// installed package before offering the prompt.
    pub fn author_fingerprint(&self) -> Result<[u8; 20], SignError> {
        let pem = fs::read(&self.author_cert).map_err(SignError::Io)?;
        let der = certificate_der(&pem).map_err(|_| {
            SignError::Keys(format!(
                "{} is not a certificate",
                self.author_cert.display()
            ))
        })?;
        Ok(sha1_digest(&der))
    }
}

#[derive(Debug)]
pub enum SignError {
    Io(io::Error),
    BadPackage(String),
    Keys(String),
    Tool(String),
}

impl std::fmt::Display for SignError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "cannot prepare package for signing: {error}"),
            Self::BadPackage(detail) => write!(formatter, "cannot unpack package: {detail}"),
            Self::Keys(detail) => write!(formatter, "signing keys unavailable: {detail}"),
            Self::Tool(detail) => write!(formatter, "xmlsec1 failed: {detail}"),
        }
    }
}

#[derive(Debug)]
pub struct SignOutcome {
    pub files: usize,
}

/// Replace the package's existing signatures with a fresh author + distributor
/// signature, then rewrite the archive in place.
pub fn resign(
    staged: &Path,
    pkg_type: PackageType,
    keys: &SigningKeys,
) -> Result<SignOutcome, SignError> {
    let work = staged
        .parent()
        .ok_or_else(|| SignError::Keys("staged package has no directory".into()))?
        .join("sign");
    if work.exists() {
        fs::remove_dir_all(&work).map_err(SignError::Io)?;
    }
    fs::create_dir(&work).map_err(SignError::Io)?;
    let result = resign_in(&work, staged, pkg_type, keys);
    let _ = fs::remove_dir_all(&work);
    result
}

fn resign_in(
    work: &Path,
    staged: &Path,
    pkg_type: PackageType,
    keys: &SigningKeys,
) -> Result<SignOutcome, SignError> {
    let mut files = extract(work, staged)?;
    // Old signatures must not be carried over: the new ones cover the payload.
    files.retain(|name| !is_signature_file(name));
    for name in ["author-signature.xml", "signature1.xml"] {
        let _ = fs::remove_file(work.join(name));
    }
    if files.is_empty() {
        return Err(SignError::BadPackage("package has no payload files".into()));
    }
    for name in signature_files(&files) {
        let _ = fs::remove_file(work.join(name));
    }

    // Author signature: every payload file.
    let author_template = work.join("author.template.xml");
    fs::write(&author_template, template(Role::Author, &files)).map_err(SignError::Io)?;
    let author_signed = sign(keys, Role::Author, &author_template, work)?;
    fs::write(work.join("author-signature.xml"), author_signed).map_err(SignError::Io)?;
    let _ = fs::remove_file(&author_template);

    // Distributor signature: the author signature plus every payload file.
    let mut distributor_refs = vec!["author-signature.xml".to_owned()];
    distributor_refs.extend(files.iter().cloned());
    let distributor_template = work.join("distributor.template.xml");
    fs::write(
        &distributor_template,
        template(Role::Distributor, &distributor_refs),
    )
    .map_err(SignError::Io)?;
    let distributor_signed = sign(keys, Role::Distributor, &distributor_template, work)?;
    fs::write(work.join("signature1.xml"), distributor_signed).map_err(SignError::Io)?;
    let _ = fs::remove_file(&distributor_template);

    let mut archive_files = files.clone();
    archive_files.push("author-signature.xml".to_owned());
    archive_files.push("signature1.xml".to_owned());
    archive_files.sort();
    archive_files.dedup();
    write_archive(work, staged, &archive_files)?;

    let _ = pkg_type;
    Ok(SignOutcome {
        files: archive_files.len(),
    })
}

/// Files whose names are Tizen signatures (`author-signature.xml`,
/// `signatureN.xml`), at the archive root.
pub fn is_signature_file(name: &str) -> bool {
    if name == "author-signature.xml" {
        return true;
    }
    let Some(stem) = name.strip_prefix("signature") else {
        return false;
    };
    let Some(number) = stem.strip_suffix(".xml") else {
        return false;
    };
    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
}

fn signature_files(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|name| is_signature_file(name))
        .cloned()
        .collect()
}

/// Unpack the archive into `work`, returning the sorted payload file names.
fn extract(work: &Path, staged: &Path) -> Result<Vec<String>, SignError> {
    let file = File::open(staged).map_err(SignError::Io)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| SignError::BadPackage(format!("not a readable zip archive: {error}")))?;
    let mut names = BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| SignError::BadPackage(format!("cannot read entry: {error}")))?;
        let name = entry.name().to_owned();
        if !is_safe_entry_name(&name) {
            return Err(SignError::BadPackage(format!("unsafe entry name: {name}")));
        }
        if entry.is_dir() {
            fs::create_dir_all(work.join(&name)).map_err(SignError::Io)?;
            continue;
        }
        if names.len() >= MAX_SIGN_FILES {
            return Err(SignError::BadPackage("too many files".into()));
        }
        total = total.saturating_add(entry.size());
        if total > MAX_SIGN_BYTES {
            return Err(SignError::BadPackage(
                "uncompressed size is too large".into(),
            ));
        }
        let destination = work.join(&name);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(SignError::Io)?;
        }
        let mut output = File::create(&destination).map_err(SignError::Io)?;
        io::copy(&mut entry, &mut output).map_err(SignError::Io)?;
        names.insert(name);
    }
    Ok(names.into_iter().collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Author,
    Distributor,
}

impl Role {
    fn id(self) -> &'static str {
        match self {
            Self::Author => "AuthorSignature",
            Self::Distributor => "DistributorSignature",
        }
    }

    fn uri(self) -> &'static str {
        match self {
            Self::Author => "http://www.w3.org/ns/widgets-digsig#role-author",
            Self::Distributor => "http://www.w3.org/ns/widgets-digsig#role-distributor",
        }
    }

    fn key_spec(self, keys: &SigningKeys) -> OsString {
        let (key, cert, ca) = match self {
            Self::Author => (&keys.author_key, &keys.author_cert, &keys.author_ca),
            Self::Distributor => (
                &keys.distributor_key,
                &keys.distributor_cert,
                &keys.distributor_ca,
            ),
        };
        let mut spec = key.clone().into_os_string();
        spec.push(",");
        spec.push(cert);
        spec.push(",");
        spec.push(ca);
        spec
    }
}

/// Signature template. Digest values are left empty: `xmlsec1` computes them
/// (including `#prop`, canonicalized per c14n 1.1) before signing.
fn template(role: Role, references: &[String]) -> String {
    let target = format!("#{}", role.id());
    let mut xml = String::new();
    xml.push_str(&format!(
        "<Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\" Id=\"{}\">\n<SignedInfo>\n",
        role.id()
    ));
    xml.push_str(
        "<CanonicalizationMethod Algorithm=\"http://www.w3.org/2001/10/xml-exc-c14n#\"></CanonicalizationMethod>\n",
    );
    xml.push_str(
        "<SignatureMethod Algorithm=\"http://www.w3.org/2001/04/xmldsig-more#rsa-sha512\"></SignatureMethod>\n",
    );
    for reference in references {
        xml.push_str(&format!(
            "<Reference URI=\"{}\">\n\
             <DigestMethod Algorithm=\"http://www.w3.org/2001/04/xmlenc#sha512\"></DigestMethod>\n\
             <DigestValue></DigestValue>\n\
             </Reference>\n",
            encode_reference(reference)
        ));
    }
    xml.push_str(
        "<Reference URI=\"#prop\">\n\
         <Transforms><Transform Algorithm=\"http://www.w3.org/2006/12/xml-c14n11\"></Transform></Transforms>\n\
         <DigestMethod Algorithm=\"http://www.w3.org/2001/04/xmlenc#sha512\"></DigestMethod>\n\
         <DigestValue></DigestValue>\n\
         </Reference>\n\
         </SignedInfo>\n\
         <SignatureValue></SignatureValue>\n\
         <KeyInfo><X509Data><X509Certificate></X509Certificate></X509Data></KeyInfo>\n",
    );
    xml.push_str(
        "<Object Id=\"prop\"><SignatureProperties xmlns:dsp=\"http://www.w3.org/2009/xmldsig-properties\">",
    );
    xml.push_str(&format!(
        "<SignatureProperty Id=\"profile\" Target=\"{target}\">\
         <dsp:Profile URI=\"http://www.w3.org/ns/widgets-digsig#profile\"></dsp:Profile>\
         </SignatureProperty>\
         <SignatureProperty Id=\"role\" Target=\"{target}\">\
         <dsp:Role URI=\"{}\"></dsp:Role>\
         </SignatureProperty>\
         <SignatureProperty Id=\"identifier\" Target=\"{target}\">\
         <dsp:Identifier></dsp:Identifier>\
         </SignatureProperty></SignatureProperties></Object>\n</Signature>\n",
        role.uri()
    ));
    xml
}

/// RFC 3986 percent-encoding for a Reference URI (Tizen encodes `/` as `%2F`).
pub fn encode_reference(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn sign(
    keys: &SigningKeys,
    role: Role,
    template: &Path,
    work: &Path,
) -> Result<Vec<u8>, SignError> {
    let spec = role.key_spec(keys);
    let outcome = executor::execute_argv_in(
        &keys.xmlsec1,
        &[
            OsStr::new("sign"),
            OsStr::new("--id-attr"),
            OsStr::new("Id"),
            OsStr::new("--privkey-pem"),
            spec.as_os_str(),
            template.as_os_str(),
        ],
        work,
        SIGN_TIMEOUT_MS,
    )
    .map_err(SignError::Io)?;
    if outcome.timed_out {
        return Err(SignError::Tool("xmlsec1 timed out".into()));
    }
    if outcome.exit_code != 0 || outcome.stdout.is_empty() {
        let mut detail = String::from_utf8_lossy(&outcome.stderr).trim().to_owned();
        if detail.is_empty() {
            detail = format!("xmlsec1 exited {}", outcome.exit_code);
        }
        return Err(SignError::Tool(detail));
    }
    Ok(strip_declaration(outcome.stdout))
}

/// Tizen's signature files start directly with `<Signature>`; drop xmlsec1's
/// `<?xml ...?>` line.
fn strip_declaration(mut xml: Vec<u8>) -> Vec<u8> {
    if xml.starts_with(XML_DECLARATION.as_bytes()) {
        if let Some(newline) = xml.iter().position(|byte| *byte == b'\n') {
            xml.drain(..=newline);
        }
    }
    xml
}

fn write_archive(work: &Path, staged: &Path, files: &[String]) -> Result<(), SignError> {
    let target = staged.with_extension("signed");
    let file = File::create(&target).map_err(SignError::Io)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for name in files {
        let mut source = File::open(work.join(name)).map_err(SignError::Io)?;
        writer
            .start_file(name.as_str(), options)
            .map_err(|error| SignError::BadPackage(format!("cannot write archive: {error}")))?;
        io::copy(&mut source, &mut writer).map_err(SignError::Io)?;
    }
    writer
        .finish()
        .map_err(|error| SignError::BadPackage(format!("cannot finish archive: {error}")))?;
    fs::rename(&target, staged).map_err(SignError::Io)?;
    Ok(())
}

/// SHA-1 of the author certificate embedded in an installed package, used for
/// Tizen's same-author upgrade rule. Returns `None` when the package has no
/// readable author signature.
pub fn installed_author_fingerprint(pkgid: &str, roots: &[PathBuf]) -> Option<[u8; 20]> {
    if !crate::identity::is_valid_package_id(pkgid) {
        return None;
    }
    for root in roots {
        for relative in ["author-signature.xml", "res/wgt/author-signature.xml"] {
            let path = root.join(pkgid).join(relative);
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(der) = first_certificate(&text) {
                return Some(sha1_digest(&der));
            }
        }
    }
    None
}

/// First `<X509Certificate>` payload in a signature XML as DER.
fn first_certificate(xml: &str) -> Option<Vec<u8>> {
    let start = xml.find("<X509Certificate>")? + "<X509Certificate>".len();
    let rest = &xml[start..];
    let end = rest.find("</X509Certificate>")?;
    let compact: String = rest[..end]
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_reference_uris_like_tizen() {
        assert_eq!(encode_reference("bin%2F"), "bin%252F");
        assert_eq!(encode_reference("bin/TvAuthUi.dll"), "bin%2FTvAuthUi.dll");
        assert_eq!(
            encode_reference("shared/res/org.example.png"),
            "shared%2Fres%2Forg.example.png"
        );
        assert_eq!(encode_reference("a b.xml"), "a%20b.xml");
    }

    #[test]
    fn recognises_signature_files() {
        assert!(is_signature_file("author-signature.xml"));
        assert!(is_signature_file("signature1.xml"));
        assert!(is_signature_file("signature12.xml"));
        assert!(!is_signature_file("signature.xml"));
        assert!(!is_signature_file("signature1.xml.bak"));
        assert!(!is_signature_file("res/signature1.xml"));
    }

    #[test]
    fn author_and_distributor_templates_match_the_reference_shape() {
        let files = vec!["config.xml".to_owned(), "js/app.js".to_owned()];
        let author = template(Role::Author, &files);
        assert!(author.starts_with(
            "<Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\" Id=\"AuthorSignature\">\n"
        ));
        assert!(author.contains("URI=\"config.xml\""));
        assert!(author.contains("URI=\"js%2Fapp.js\""));
        assert!(author.contains(
            "Id=\"role\" Target=\"#AuthorSignature\">\
            <dsp:Role URI=\"http://www.w3.org/ns/widgets-digsig#role-author\">\
            </dsp:Role>"
        ));
        assert!(author.contains("<Reference URI=\"#prop\">"));
        assert!(author.contains("http://www.w3.org/2006/12/xml-c14n11"));
        assert!(author.contains("rsa-sha512"));
        assert!(!author.contains("author-signature.xml"));

        let distributor = template(Role::Distributor, &files);
        assert!(distributor.contains("Id=\"DistributorSignature\""));
        assert!(distributor.contains("role-distributor"));
        assert!(distributor.contains("Target=\"#DistributorSignature\""));
    }

    #[test]
    fn strips_the_xml_declaration() {
        assert_eq!(
            strip_declaration(b"<?xml version=\"1.0\"?>\n<Signature/>".to_vec()),
            b"<Signature/>".to_vec()
        );
        assert_eq!(
            strip_declaration(b"<Signature/>".to_vec()),
            b"<Signature/>".to_vec()
        );
    }

    #[test]
    fn extracts_the_first_certificate() {
        let xml = "<X509Data><X509Certificate>YWJj\nZA==</X509Certificate>\
                   <X509Certificate>eA==</X509Certificate></X509Data>";
        assert_eq!(first_certificate(xml).unwrap(), b"abcd");
        assert!(first_certificate("<X509Data></X509Data>").is_none());
    }
}
