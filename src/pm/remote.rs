//! The remote coven registry client: speaks the JSON wire protocol (see
//! [`super::wire`]) to a `coven serve` HTTP server. It performs the same
//! zero-trust verification a local registry does — every fetched record's
//! signature is checked against the registry's pinned root key, and every
//! fetched source is checked against the signed hash — so the network is never
//! trusted, only verified.

use std::cell::RefCell;
use std::collections::BTreeSet;

use super::footprint::Widening;
use super::manifest::Manifest;
use super::registry::{verify_record_with, Promotion, Record};
use super::store::RuneSource;
use super::wire::*;
use super::{err, http, PmResult};

pub struct RemoteRegistry {
    base: String,
    root_pub: RefCell<Option<String>>,
}

impl RemoteRegistry {
    pub fn new(base_url: String) -> RemoteRegistry {
        let base = base_url.trim_end_matches('/').to_string();
        RemoteRegistry {
            base,
            root_pub: RefCell::new(None),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub fn root_public_hex(&self) -> PmResult<String> {
        if let Some(p) = self.root_pub.borrow().clone() {
            return Ok(p);
        }
        let resp = http::get(&self.url("/coven/rootpub"))?;
        if !resp.is_ok() {
            return err(format!("registry has no published root key (HTTP {})", resp.status));
        }
        let pubhex = String::from_utf8_lossy(&resp.body).trim().to_string();
        *self.root_pub.borrow_mut() = Some(pubhex.clone());
        Ok(pubhex)
    }

    pub fn list_all(&self) -> PmResult<Vec<String>> {
        let r: IndexResp = http::get_json(&self.url("/coven/index"))?;
        Ok(r.names)
    }

    pub fn tuf_timestamp(
        &self,
    ) -> PmResult<super::tuf::Signed<super::tuf::Timestamp>> {
        http::get_json(&self.url("/coven/timestamp"))
    }

    pub fn tuf_snapshot(&self) -> PmResult<super::tuf::Signed<super::tuf::Snapshot>> {
        http::get_json(&self.url("/coven/snapshot"))
    }

    pub fn versions(&self, name: &str) -> Vec<Record> {
        let url = self.url(&format!("/coven/versions?name={}", wire_name(name)));
        http::get_json::<VersionsResp>(&url)
            .map(|r| r.records)
            .unwrap_or_default()
    }

    pub fn record(&self, name: &str, version: &str) -> PmResult<Record> {
        let url = self.url(&format!(
            "/coven/record?name={}&version={version}",
            wire_name(name)
        ));
        http::get_json(&url)
    }

    pub fn fetch(&self, name: &str, version: &str) -> PmResult<RuneSource> {
        let record = self.record(name, version)?;
        // Zero-trust: verify the record's signature against the pinned root key
        // before trusting any field, then confirm the source matches the signed
        // hash.
        let pubkey = self.root_public_hex()?;
        verify_record_with(&pubkey, &record)?;
        let url = self.url(&format!(
            "/coven/source?name={}&version={version}",
            wire_name(name)
        ));
        let dto: SourceDto = http::get_json(&url)?;
        let src = dto.into_source();
        if src.hash() != record.hash {
            return err(format!(
                "integrity failure: {name}@{version} source hashes to {} but the signed record says {}",
                src.hash(),
                record.hash
            ));
        }
        Ok(src)
    }

    pub fn publish(
        &self,
        src: &RuneSource,
        manifest: &Manifest,
        uploaded_by: &str,
        token: &str,
    ) -> PmResult<Record> {
        let manifest_toml = src
            .files
            .iter()
            .find(|(p, _)| p == super::manifest::MANIFEST_NAME)
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| {
                // Fall back to re-serializing if the source somehow lacks it.
                toml::to_string_pretty(manifest).unwrap_or_default()
            });
        let req = PublishReq {
            manifest_toml,
            source: SourceDto::from_source(src),
            uploaded_by: uploaded_by.to_string(),
            token: token.to_string(),
        };
        http::post(&self.url("/coven/publish"), &req)
    }

    pub fn promote(
        &self,
        name: &str,
        version: &str,
        promoter: &str,
        second_factor: &str,
        token: &str,
    ) -> PmResult<Promotion> {
        let req = PromoteReq {
            name: name.to_string(),
            version: version.to_string(),
            promoter: promoter.to_string(),
            second_factor: second_factor.to_string(),
            token: token.to_string(),
        };
        let resp: PromoteResp = http::post(&self.url("/coven/promote"), &req)?;
        Ok(Promotion {
            record: resp.record,
            footprint_delta: Widening {
                runtime: resp.delta_runtime.into_iter().collect::<BTreeSet<_>>(),
                build: resp.delta_build.into_iter().collect::<BTreeSet<_>>(),
            },
            separation_of_duties: resp.separation_of_duties,
        })
    }

    pub fn yank(&self, name: &str, version: &str, token: &str) -> PmResult<()> {
        let req = YankReq {
            name: name.to_string(),
            version: version.to_string(),
            token: token.to_string(),
        };
        let _: OkResp = http::post(&self.url("/coven/yank"), &req)?;
        Ok(())
    }
}
