//! Strict bounded wire identities for the nonpublishing body-interface experiment.
use std::collections::BTreeMap;
pub(super) const MODE: &str = "BODY_INTERFACE_REUSE_DIAGNOSTIC";
// Keep in sync with operationLimit in the V2 Lean helper.
pub(super) const BODY_OPERATIONS: usize = 16;

pub(super) const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
pub(super) fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v2_binds_every_field_and_rejects_extra_duplicate_or_escaped_fields() {
        let nonce = "1".repeat(32);
        let r = Request { binding: ZERO, nonce: &nonce, seq: 2, op: "CHECK", context: ZERO,
            source: ZERO, module: ZERO, body: ZERO, bytes: 40, marker: "CURRENT:1" };
        assert!(r.header().is_some());
        let line = format!("WITCHY_RESIDENT_V2:{{\"v\":2,\"phase\":\"CHECK\",\"status\":\"ok\",\"binding\":\"{ZERO}\",\"nonce\":\"{nonce}\",\"seq\":2,\"context\":\"{ZERO}\",\"source\":\"{ZERO}\",\"module\":\"{ZERO}\",\"body\":\"{ZERO}\",\"marker\":\"CURRENT:1\",\"mode\":\"{MODE}\"}}");
        assert!(r.reply(&line, "ok"));
        for bad in [line.replace("\"v\":2", "\"v\":\"2\""),
            line.replace("\"seq\":2", "\"seq\":3"),
            line.replace("\"seq\":2", "\"seq\":2,\"seq\":2"),
            line.replace("\"seq\":2", "\"seq\":2,\"other\":0"),
            line.replace("CURRENT:1", "CURRENT:2"),
            line.replace("\"CHECK\"", "\"INSTALL_BODY\""),
            line.replace("\"ok\"", "\"error\""),
            line.replace("CURRENT", "C\\u0055RRENT"),
            line.replace(&format!("\"source\":\"{ZERO}\""), &format!("\"source\":\"{}\"", "a".repeat(64)))] {
            assert!(!r.reply(&bad, "ok"));
        }
    }
}
pub(super) struct Request<'a> {
    pub binding: &'a str, pub nonce: &'a str, pub seq: usize, pub op: &'a str,
    pub context: &'a str, pub source: &'a str, pub module: &'a str,
    pub body: &'a str, pub bytes: usize, pub marker: &'a str,
}
impl Request<'_> {
    pub fn header(&self) -> Option<String> {
        if ![self.binding, self.context, self.source, self.module, self.body].into_iter().all(hash)
            || self.nonce.len() != 32 || !self.nonce.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.seq > BODY_OPERATIONS + 1 || self.bytes > 65536 || self.marker.len() > 512
            || !self.marker.bytes().all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b))
            || !matches!(self.op, "LOAD" | "QUEUE" | "INSTALL_BODY" | "CHECK" | "STOP") { return None; }
        if matches!(self.op, "LOAD" | "STOP") && (self.bytes != 0 || self.source != ZERO
            || self.module != ZERO || self.body != ZERO || !self.marker.is_empty()) { return None; }
        if matches!(self.op, "QUEUE" | "INSTALL_BODY" | "CHECK") && (self.bytes == 0 || self.marker.is_empty()) { return None; }
        Some(format!("{{\"v\":2,\"op\":\"{}\",\"binding\":\"{}\",\"nonce\":\"{}\",\"seq\":{},\"context\":\"{}\",\"source\":\"{}\",\"module\":\"{}\",\"body\":\"{}\",\"bytes\":{},\"marker\":\"{}\"}}\n",
            self.op, self.binding, self.nonce, self.seq, self.context, self.source, self.module, self.body, self.bytes, self.marker))
    }
    pub fn reply(&self, line: &str, status: &str) -> bool {
        let Some(json) = line.strip_prefix("WITCHY_RESIDENT_V2:{").and_then(|s| s.strip_suffix('}')) else { return false; };
        let mut fields = BTreeMap::new();
        for field in json.split(',') {
            let Some((key, value)) = field.split_once(':') else { return false; };
            let Some(key) = key.trim().strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return false; };
            let value = value.trim();
            let value = if matches!(key, "v" | "seq") { value } else {
                let Some(value) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else { return false; };
                if value.bytes().any(|b| b < 32 || b == b'"' || b == b'\\') { return false; }
                value
            };
            if fields.insert(key, value.to_owned()).is_some() { return false; }
        }
        fields == [("v", "2".into()), ("phase", self.op.into()), ("status", status.into()),
            ("binding", self.binding.into()), ("nonce", self.nonce.into()), ("seq", self.seq.to_string()),
            ("context", self.context.into()), ("source", self.source.into()), ("module", self.module.into()),
            ("body", self.body.into()), ("marker", self.marker.into()), ("mode", MODE.into())].into_iter().collect()
    }
}
