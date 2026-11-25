//! Trusted operator build record, not a signature or filesystem immutability proof.
use super::proof_digest as digest;
use crate::diagnostic::Diagnostic;
use std::{fs, io::Read, path::{Path, PathBuf}};
type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message: &'static str) -> Diagnostic { Diagnostic::new("W0402", 0..0, message) }
const POLICY: &str = "selected_readonly_generation_no_concurrent_mutation";
#[derive(Clone)]
pub(super) struct Binding {
    pub fixed_header: bool,
    pub path: PathBuf,
    pub identity: String,
    pub helper: PathBuf,
    pub helper_id: String,
    pub root: PathBuf,
    pub sdk: String,
}
fn contents(path: &Path) -> Result<String> {
    if !path.is_absolute() || fs::canonicalize(path).ok().as_deref() != Some(path)
        || !fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return Err(error("ready binding must be an absolute canonical operator path"));
    }
    let mut text = String::new();
    fs::File::open(path).and_then(|f| f.take(8193).read_to_string(&mut text))
        .map_err(|_| error("ready binding read refused"))?;
    if text.len() > 8192 { return Err(error("ready binding exceeds 8 KiB")); }
    Ok(text)
}
fn parse(text: &str) -> Result<([&str; 7], Option<&str>)> {
    let lines: Vec<_> = text.lines().collect();
    let packed = lines.first() == Some(&"witchy.ready-toolchain.v2");
    if !text.ends_with('\n') || !(packed && lines.len() == 9 ||
        lines.first() == Some(&"witchy.ready-toolchain.v1") && lines.len() == 8) {
        return Err(error("ready binding version/shape refused"));
    }
    let keys = ["helper=", "helper_sha256=", "sdk=", "sdk_sha256=", "helper_mode=", "source_sha256=", "trust_policy="];
    let mut values = [""; 7];
    for (i, key) in keys.iter().enumerate() {
        values[i] = lines[i + 1].strip_prefix(key).filter(|v| !v.is_empty() && !v.chars().any(char::is_control))
            .ok_or_else(|| error("ready binding field/order refused"))?;
    }
    if !Path::new(values[0]).is_absolute() || !Path::new(values[2]).is_absolute()
        || ![values[1], values[3], values[5]].into_iter().all(super::proof_wire2::hash)
        || values[4] != "body-interface-v2" || values[6] != POLICY {
        return Err(error("ready binding identity/mode/policy refused"));
    }
    let abi = if packed {
        let id = lines[8].strip_prefix("darwin_shared_cache_uuid=").unwrap_or("");
        let valid = id.len() == 36 && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() && !b.is_ascii_uppercase() }
        });
        if !valid { return Err(error("invalid fixed-header ABI")); }
        Some(id)
    } else { None };
    Ok((values, abi))
}
// A serialized native closure also depends on the exact system libraries.
#[cfg(target_os = "macos")]
fn shared_cache_uuid() -> Option<String> {
    unsafe extern "C" { fn _dyld_get_shared_cache_uuid(uuid: *mut u8) -> bool; }
    let mut id = [0u8; 16];
    if !unsafe { _dyld_get_shared_cache_uuid(id.as_mut_ptr()) } { return None; }
    let mut text = String::with_capacity(36);
    use std::fmt::Write;
    for (i, b) in id.iter().enumerate() {
        if [4, 6, 8, 10].contains(&i) { text.push('-'); }
        write!(&mut text, "{b:02x}").ok()?;
    }
    Some(text)
}
#[cfg(not(target_os = "macos"))]
fn shared_cache_uuid() -> Option<String> { None }

impl Binding {
    pub fn configured() -> Result<Option<Self>> {
        std::env::var_os("WITCHY_WASM_READY_BINDING").map(|p| Self::load(Path::new(&p))).transpose()
    }
    pub(super) fn load(path: &Path) -> Result<Self> {
        let text = contents(path)?;
        let (v, abi) = parse(&text)?;
        if abi.is_some() && abi != shared_cache_uuid().as_deref() {
            return Err(error("Darwin shared cache changed; reinstall the fixed-header checker"));
        }
        let source = [include_bytes!("../../tools/proof-session-body/Resident.lean").as_slice(), b"\0",
            include_bytes!("../../tools/proof-session/native.rs").as_slice(), b"\0",
            include_bytes!("proof_audit.lean.in").as_slice()].concat();
        // Existing installed v2 helpers have the same wire and admission checks.
        const SERIAL_V2: &str = "3a03a52e04eff6139819aa3a189794d47418ca7afec1e1320512c07136932ea8";
        if v[5] != SERIAL_V2 && digest::bytes(&source).map_err(|_| error("ready helper source identity I/O"))? != v[5] {
            return Err(error("ready helper source differs from this compiler"));
        }
        Ok(Self { fixed_header: abi.is_some(), path: path.into(), identity: digest::bytes(text.as_bytes()).map_err(|_| error("ready binding identity I/O"))?,
            helper: v[0].into(), helper_id: v[1].into(), root: v[2].into(), sdk: v[3].into() })
    }
    pub fn validate(&self) -> Result<()> {
        let text = contents(&self.path)?;
        if digest::bytes(text.as_bytes()).map_err(|_| error("ready binding identity I/O"))? != self.identity
            || fs::canonicalize(&self.root).ok().as_ref() != Some(&self.root)
            || fs::canonicalize(&self.helper).ok().as_ref() != Some(&self.helper)
            || !fs::symlink_metadata(&self.helper).is_ok_and(|m| m.is_file())
            || !fs::metadata(&self.root).is_ok_and(|m| m.is_dir())
            || digest::file(&self.helper).map_err(|_| error("ready helper identity I/O"))? != self.helper_id {
            return Err(error("selected ready binding/root/helper changed; restart with a new generation"));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{proof_measure::Measurements, proof_session::Session};

    #[test]
    fn a_selected_binding_cannot_switch_generations_before_its_first_request() {
        let path = std::env::temp_dir().canonicalize().unwrap()
            .join(format!("witchy-selected-binding-{}", std::process::id()));
        let mut file = fs::File::create_new(&path).unwrap();
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
        }
        let _remove = Remove(path.clone());
        let binding_text = format!(
            "witchy.ready-toolchain.v1\nhelper=/helper\nhelper_sha256={}\nsdk=/sdk\nsdk_sha256={}\nhelper_mode=body-interface-v2\nsource_sha256=3a03a52e04eff6139819aa3a189794d47418ca7afec1e1320512c07136932ea8\ntrust_policy={POLICY}\n",
            "1".repeat(64), "2".repeat(64),
        );
        std::io::Write::write_all(&mut file, binding_text.as_bytes()).unwrap();
        let selected = Binding::load(&path).unwrap();
        fs::write(&path, binding_text.replace("sdk=/sdk", "sdk=/another-sdk")).unwrap();
        let replaced = Binding::load(&path).unwrap();
        assert_ne!(selected.identity, replaced.identity);

        let mut session = Session::for_binding(selected).unwrap();
        let mut measurements = Measurements::default();
        let error = session.run("", "", "", &mut measurements).unwrap_err();
        assert_eq!(error.message, "selected ready binding/root/helper changed; restart with a new generation");
        assert_eq!(measurements.installation_snapshots, 0);
    }

    #[test]
    fn binding_rejects_ambiguous_fields_and_wrong_policy() {
        let good = format!("witchy.ready-toolchain.v1\nhelper=/helper\nhelper_sha256={}\nsdk=/sdk\nsdk_sha256={}\nhelper_mode=body-interface-v2\nsource_sha256={}\ntrust_policy={POLICY}\n", "1".repeat(64), "2".repeat(64), "3".repeat(64));
        assert!(parse(&good).is_ok());
        for bad in [good.replace("sdk=/sdk", "sdk=relative"), good.replace(POLICY, "immutable"),
            good.replace("helper_mode=body-interface-v2", "helper_mode=anything"),
            format!("{good}sdk=/other\n"), good.trim_end().into(), good.replace("helper=/helper", "helper=/hel\rper")] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }
}
