use std::collections::{BTreeMap, BTreeSet};

use witchy_cap_model::{
    FetchOrigin, ParsedFetchUrl, validate_fetch_header, validate_fetch_method,
    validate_http_header_syntax,
};

use crate::{
    FixtureCall, FixtureErrorCode, FixtureFailure, FixtureFamily, FixtureFetchRequest,
    FixtureFetchResponse, FixtureHandle, FixtureOutcome, FixturePlan, FixtureSession, FixtureValue,
    SourceLocation,
};
use crate::hex::{decode as decode_hex, encode as encode_hex};

pub type FetchProviderResult<T> = Result<T, FixtureFailure>;

#[derive(Debug)]
pub(crate) struct FetchProviderState {
    configured: bool,
    root_origins: BTreeSet<String>,
    handles: BTreeMap<u64, BTreeSet<String>>,
}

impl FetchProviderState {
    pub(crate) fn new(plan: &FixturePlan) -> Self {
        let Some(fixture) = &plan.fetch else {
            return Self {
                configured: false,
                root_origins: BTreeSet::new(),
                handles: BTreeMap::new(),
            };
        };
        Self {
            configured: true,
            root_origins: fixture.origins.iter().cloned().collect(),
            handles: BTreeMap::new(),
        }
    }

    pub(crate) const fn configured(&self) -> bool {
        self.configured
    }
}

impl FixtureSession {
    pub fn mint_fixture_fetch(
        &mut self,
        source: Option<SourceLocation>,
    ) -> FetchProviderResult<FixtureHandle> {
        if !self.fetch.configured {
            return Err(fetch_failure(
                FixtureErrorCode::Denied,
                "Fetch fixture was not declared",
            ));
        }
        let mut call = FixtureCall::new(FixtureFamily::Fetch, "mint_fetch");
        call.effective_rights = self.fetch.root_origins.iter().cloned().collect();
        call.source = source;
        marker(
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Fetch".into()),
                },
            ),
            "mint_fetch",
        )?;
        let handle = self
            .basic
            .mint_handle(FixtureFamily::Fetch, BTreeSet::new());
        self.fetch
            .handles
            .insert(handle.id(), self.fetch.root_origins.clone());
        Ok(handle)
    }

    pub fn fetch_only(
        &mut self,
        handle: &FixtureHandle,
        origins: &[String],
        source: Option<SourceLocation>,
    ) -> FetchProviderResult<FixtureHandle> {
        let current = self.fetch_origins(handle)?;
        let mut narrowed = BTreeSet::new();
        for origin in origins {
            let parsed = FetchOrigin::parse(origin).map_err(|error| {
                fetch_failure(FixtureErrorCode::InvalidRequest, error.to_string())
            })?;
            let canonical = parsed.as_str();
            if !current.contains(&canonical) {
                return Err(fetch_failure(
                    FixtureErrorCode::Denied,
                    format!("Fetch origin `{canonical}` is not granted"),
                ));
            }
            narrowed.insert(canonical);
        }
        let mut call = FixtureCall::new(FixtureFamily::Fetch, "fetch_only");
        call.arguments.insert(
            "origins".into(),
            FixtureValue::List(
                origins
                    .iter()
                    .cloned()
                    .map(FixtureValue::String)
                    .collect(),
            ),
        );
        call.effective_rights = current.iter().cloned().collect();
        call.source = source;
        marker(
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("Fetch".into()),
                },
            ),
            "fetch_only",
        )?;
        let narrowed_handle = self
            .basic
            .mint_handle(FixtureFamily::Fetch, BTreeSet::new());
        self.fetch.handles.insert(narrowed_handle.id(), narrowed);
        Ok(narrowed_handle)
    }

    pub fn fetch_send(
        &mut self,
        handle: &FixtureHandle,
        request: &FixtureFetchRequest,
        source: Option<SourceLocation>,
    ) -> FetchProviderResult<FixtureFetchResponse> {
        let origins = self.fetch_origins(handle)?;
        validate_fetch_method(&request.method)
            .map_err(|error| fetch_failure(FixtureErrorCode::InvalidRequest, error.to_string()))?;
        for (name, value) in &request.headers {
            validate_fetch_header(name, value).map_err(|error| {
                fetch_failure(FixtureErrorCode::InvalidRequest, error.to_string())
            })?;
        }
        let parsed = ParsedFetchUrl::parse(&request.url, false)
            .map_err(|error| fetch_failure(FixtureErrorCode::InvalidRequest, error.to_string()))?;
        let request_origin = parsed.origin().as_str();
        if !origins.contains(&request_origin) {
            return Err(fetch_failure(
                FixtureErrorCode::Denied,
                format!("Fetch origin `{request_origin}` is not granted"),
            ));
        }

        let mut call = FixtureCall::new(FixtureFamily::Fetch, "fetch_send_len");
        call.target = Some(request.url.clone());
        call.arguments.insert(
            "method".into(),
            FixtureValue::String(request.method.clone()),
        );
        call.arguments.insert(
            "headers".into(),
            FixtureValue::List(
                request
                    .headers
                    .iter()
                    .map(|(name, value)| {
                        FixtureValue::Map(BTreeMap::from([
                            ("name".into(), FixtureValue::String(name.clone())),
                            ("value".into(), FixtureValue::String(value.clone())),
                        ]))
                    })
                    .collect(),
            ),
        );
        call.arguments.insert(
            "body".into(),
            FixtureValue::Bytes(encode_hex(&request.body)),
        );
        call.effective_rights = origins.iter().cloned().collect();
        call.source = source;
        decode_response(self.scripted_call(call))
    }

    fn fetch_origins(
        &self,
        handle: &FixtureHandle,
    ) -> FetchProviderResult<BTreeSet<String>> {
        if !self.basic.validate_handle(handle, FixtureFamily::Fetch) {
            return Err(fetch_failure(
                FixtureErrorCode::Denied,
                "invalid or foreign Fetch fixture handle",
            ));
        }
        self.fetch
            .handles
            .get(&handle.id())
            .cloned()
            .ok_or_else(|| {
                fetch_failure(
                    FixtureErrorCode::Denied,
                    "invalid or foreign Fetch fixture handle",
                )
            })
    }
}

fn marker(outcome: FixtureOutcome, operation: &str) -> FetchProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } if value == "Fetch" => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(fetch_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid Fetch handle marker"),
        )),
    }
}

fn decode_response(outcome: FixtureOutcome) -> FetchProviderResult<FixtureFetchResponse> {
    let FixtureOutcome::Return {
        value: FixtureValue::Map(mut fields),
    } = outcome
    else {
        return match outcome {
            FixtureOutcome::Fail { error } => Err(error),
            _ => Err(fetch_failure(
                FixtureErrorCode::InvalidData,
                "Fetch fixture returned an invalid response",
            )),
        };
    };
    let status = take_string(&mut fields, "status")?
        .parse::<u16>()
        .map_err(|_| fetch_failure(FixtureErrorCode::InvalidData, "invalid Fetch status"))?;
    let body = match fields.remove("body") {
        Some(FixtureValue::Bytes(value)) => decode_hex(&value),
        _ => {
            return Err(fetch_failure(
                FixtureErrorCode::InvalidData,
                "Fetch response body must be bytes",
            ));
        }
    };
    let headers = match fields.remove("headers") {
        Some(FixtureValue::List(values)) => values
            .into_iter()
            .map(decode_header)
            .collect::<FetchProviderResult<Vec<_>>>()?,
        _ => {
            return Err(fetch_failure(
                FixtureErrorCode::InvalidData,
                "Fetch response headers must be a list",
            ));
        }
    };
    if !fields.is_empty() {
        return Err(fetch_failure(
            FixtureErrorCode::InvalidData,
            "Fetch response contains unknown fields",
        ));
    }
    Ok(FixtureFetchResponse {
        status,
        headers,
        body,
    })
}

fn decode_header(value: FixtureValue) -> FetchProviderResult<(String, String)> {
    let FixtureValue::Map(mut fields) = value else {
        return Err(fetch_failure(
            FixtureErrorCode::InvalidData,
            "Fetch response header must be a map",
        ));
    };
    let name = take_string(&mut fields, "name")?;
    let value = take_string(&mut fields, "value")?;
    if !fields.is_empty() {
        return Err(fetch_failure(
            FixtureErrorCode::InvalidData,
            "Fetch response header contains unknown fields",
        ));
    }
    validate_http_header_syntax(&name, &value)
        .map_err(|error| fetch_failure(FixtureErrorCode::InvalidData, error.to_string()))?;
    Ok((name, value))
}

fn take_string(
    fields: &mut BTreeMap<String, FixtureValue>,
    name: &str,
) -> FetchProviderResult<String> {
    match fields.remove(name) {
        Some(FixtureValue::String(value)) => Ok(value),
        _ => Err(fetch_failure(
            FixtureErrorCode::InvalidData,
            format!("Fetch response `{name}` must be a string"),
        )),
    }
}

fn fetch_failure(code: FixtureErrorCode, message: impl Into<String>) -> FixtureFailure {
    FixtureFailure {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FetchFixture, FixtureStep};

    fn response(status: &str, body: &str) -> FixtureOutcome {
        FixtureOutcome::Return {
            value: FixtureValue::Map(BTreeMap::from([
                ("status".into(), FixtureValue::String(status.into())),
                ("headers".into(), FixtureValue::List(Vec::new())),
                ("body".into(), FixtureValue::Bytes(encode_hex(body.as_bytes()))),
            ])),
        }
    }

    fn request(url: &str) -> FixtureFetchRequest {
        FixtureFetchRequest {
            method: "GET".into(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn plan(steps: Vec<FixtureStep>) -> FixturePlan {
        FixturePlan {
            fetch: Some(FetchFixture {
                origins: vec!["https://example.com:443".into()],
                script: steps,
            }),
            ..Default::default()
        }
    }

    fn step(url: &str, outcome: FixtureOutcome) -> FixtureStep {
        FixtureStep {
            operation: "fetch_send_len".into(),
            target: Some(url.into()),
            arguments: BTreeMap::from([
                ("method".into(), FixtureValue::String("GET".into())),
                ("headers".into(), FixtureValue::List(Vec::new())),
                ("body".into(), FixtureValue::Bytes(String::new())),
            ]),
            effective_rights: Some(vec!["https://example.com:443".into()]),
            outcome,
            required: true,
        }
    }

    #[test]
    fn exact_scripted_request_returns_bytes_without_network_fallback() {
        let url = "https://example.com/data";
        let mut session =
            FixtureSession::new(plan(vec![step(url, response("200", "fixture"))]))
                .expect("session");
        let fetch = session.mint_fixture_fetch(None).expect("fetch");
        let response = session
            .fetch_send(&fetch, &request(url), None)
            .expect("response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"fixture");
        assert_eq!(
            session
                .fetch_send(&fetch, &request(url), None)
                .expect_err("no fallback")
                .code,
            FixtureErrorCode::Exhausted
        );
    }

    #[test]
    fn denial_precedes_script_and_narrowing_never_widens() {
        let allowed = "https://example.com/data";
        let mut session =
            FixtureSession::new(plan(vec![step(allowed, response("200", "ok"))]))
                .expect("session");
        let fetch = session.mint_fixture_fetch(None).expect("fetch");
        assert_eq!(
            session
                .fetch_send(&fetch, &request("https://attacker.test/data"), None)
                .expect_err("denied")
                .code,
            FixtureErrorCode::Denied
        );
        assert_eq!(
            session
                .fetch_only(&fetch, &["https://attacker.test:443".into()], None)
                .expect_err("cannot widen")
                .code,
            FixtureErrorCode::Denied
        );
        assert_eq!(
            session
                .fetch_send(&fetch, &request(allowed), None)
                .expect("step retained")
                .body,
            b"ok"
        );
    }

    #[test]
    fn timeout_and_redirect_match_provider_failures() {
        let timeout_url = "https://example.com/slow";
        let redirect_url = "https://example.com/redirect";
        let mut session = FixtureSession::new(plan(vec![
            step(
                timeout_url,
                FixtureOutcome::Fail {
                    error: fetch_failure(FixtureErrorCode::Timeout, "configured timeout"),
                },
            ),
            step(
                redirect_url,
                FixtureOutcome::Fail {
                    error: fetch_failure(
                        FixtureErrorCode::Redirect,
                        "Fetch redirects are disabled (HTTP status 302)",
                    ),
                },
            ),
        ]))
        .expect("session");
        let fetch = session.mint_fixture_fetch(None).expect("fetch");
        assert_eq!(
            session
                .fetch_send(&fetch, &request(timeout_url), None)
                .expect_err("timeout")
                .code,
            FixtureErrorCode::Timeout
        );
        assert_eq!(
            session
                .fetch_send(&fetch, &request(redirect_url), None)
                .expect_err("redirect")
                .code,
            FixtureErrorCode::Redirect
        );
    }

    #[test]
    fn malformed_method_headers_and_response_fail_closed() {
        let url = "https://example.com/data";
        let mut session =
            FixtureSession::new(plan(vec![step(url, response("200", "ok"))]))
                .expect("session");
        let fetch = session.mint_fixture_fetch(None).expect("fetch");
        let mut invalid = request(url);
        invalid.method = "GE\rT".into();
        assert_eq!(
            session.fetch_send(&fetch, &invalid, None).expect_err("method").code,
            FixtureErrorCode::InvalidRequest
        );
        let mut invalid_header = request(url);
        invalid_header
            .headers
            .push(("Host".into(), "attacker.test".into()));
        assert_eq!(
            session
                .fetch_send(&fetch, &invalid_header, None)
                .expect_err("header")
                .code,
            FixtureErrorCode::InvalidRequest
        );
        assert_eq!(
            session
                .fetch_send(&fetch, &request(url), None)
                .expect("valid step remains")
                .body,
            b"ok"
        );

        let invalid_plan =
            FixtureSession::new(plan(vec![step(url, response("not-status", ""))]))
                .expect_err("malformed response plan");
        assert!(invalid_plan.message().contains("status is out of range"));
    }
}
