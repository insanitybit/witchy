//! The JSON wire protocol shared by the coven server and the remote-registry
//! client. Rune sources travel as UTF-8 text (witchy source and `witchy.toml`
//! are always text), preserving the exact bytes so content hashes round-trip.

use serde::{Deserialize, Serialize};

use super::registry::Record;
use super::store::RuneSource;

/// A rune's source for transport: (relative-path, text) pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDto {
    pub files: Vec<(String, String)>,
}

impl SourceDto {
    pub fn from_source(s: &RuneSource) -> SourceDto {
        SourceDto {
            files: s
                .files
                .iter()
                .map(|(p, b)| (p.clone(), String::from_utf8_lossy(b).into_owned()))
                .collect(),
        }
    }

    pub fn into_source(self) -> RuneSource {
        let mut files: Vec<(String, Vec<u8>)> =
            self.files.into_iter().map(|(p, c)| (p, c.into_bytes())).collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        RuneSource { files }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PublishReq {
    pub manifest_toml: String,
    pub source: SourceDto,
    pub uploaded_by: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PromoteReq {
    pub name: String,
    pub version: String,
    pub promoter: String,
    pub second_factor: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PromoteResp {
    pub record: Record,
    pub separation_of_duties: bool,
    pub delta_runtime: Vec<String>,
    pub delta_build: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct YankReq {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionsResp {
    pub records: Vec<Record>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IndexResp {
    pub names: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OkResp {
    pub ok: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrResp {
    pub error: String,
}

/// Rune names contain a `/`; encode it for use in a URL query value. Names never
/// contain `~`, so this is unambiguous and avoids percent-encoding.
pub fn wire_name(n: &str) -> String {
    n.replace('/', "~")
}

pub fn unwire_name(n: &str) -> String {
    n.replace('~', "/")
}
