use std::collections::{BTreeMap, BTreeSet};

use witchy_cap_model::USE_ONLY_SECRET_REVEAL_ERROR;

use crate::{
    FixtureCall, FixtureErrorCode, FixtureFailure, FixtureFamily, FixtureHandle, FixtureOutcome,
    FixturePlan, FixtureSession, FixtureValue, SecretUsage, SourceLocation,
};
use crate::hex::decode as decode_hex;

pub type SecretProviderResult<T> = Result<T, FixtureFailure>;

#[derive(Debug, Clone)]
struct SecretMaterial {
    bytes: Vec<u8>,
    usage: SecretUsage,
}

#[derive(Debug)]
pub(crate) struct SecretProviderState {
    configured: bool,
    entries: BTreeMap<String, SecretMaterial>,
    stores: BTreeSet<u64>,
    handles: BTreeMap<u64, SecretMaterial>,
}

impl SecretProviderState {
    pub(crate) fn new(plan: &FixturePlan) -> Self {
        let Some(fixture) = &plan.secrets else {
            return Self {
                configured: false,
                entries: BTreeMap::new(),
                stores: BTreeSet::new(),
                handles: BTreeMap::new(),
            };
        };
        Self {
            configured: true,
            entries: fixture
                .entries
                .iter()
                .map(|(name, secret)| {
                    (
                        name.clone(),
                        SecretMaterial {
                            bytes: decode_hex(&secret.hex),
                            usage: secret.usage,
                        },
                    )
                })
                .collect(),
            stores: BTreeSet::new(),
            handles: BTreeMap::new(),
        }
    }

    pub(crate) const fn configured(&self) -> bool {
        self.configured
    }
}

impl FixtureSession {
    pub fn mint_fixture_secret_store(
        &mut self,
        source: Option<SourceLocation>,
    ) -> SecretProviderResult<FixtureHandle> {
        if !self.secrets.configured {
            return Err(secret_failure(
                FixtureErrorCode::PermissionDenied,
                "SecretStore fixture was not declared",
            ));
        }
        let mut call = FixtureCall::new(FixtureFamily::SecretStore, "mint_secretstore");
        call.source = source;
        marker(
            self.observe(
                call,
                FixtureOutcome::Return {
                    value: FixtureValue::String("SecretStore".into()),
                },
            ),
            "SecretStore",
            "mint_secretstore",
        )?;
        let handle = self
            .basic
            .mint_handle(FixtureFamily::SecretStore, BTreeSet::new());
        self.secrets.stores.insert(handle.id());
        Ok(handle)
    }

    pub fn secretstore_lookup(
        &mut self,
        store: &FixtureHandle,
        name: &str,
        source: Option<SourceLocation>,
    ) -> SecretProviderResult<Option<FixtureHandle>> {
        self.validate_store(store)?;
        let mut call = FixtureCall::new(FixtureFamily::SecretStore, "secretstore_lookup");
        call.target = Some(name.into());
        call.source = source;
        let fallback = FixtureOutcome::Return {
            value: if self.secrets.entries.contains_key(name) {
                FixtureValue::String("Secret".into())
            } else {
                FixtureValue::Null
            },
        };
        let outcome = if self.has_script(FixtureFamily::SecretStore) {
            self.scripted_call(call)
        } else {
            self.observe(call, fallback)
        };
        match outcome {
            FixtureOutcome::Return {
                value: FixtureValue::Null,
            } => Ok(None),
            FixtureOutcome::Return {
                value: FixtureValue::String(kind),
            } if kind == "Secret" => {
                let material = self.secrets.entries.get(name).cloned().ok_or_else(|| {
                    secret_failure(
                        FixtureErrorCode::InvalidData,
                        format!("script returned Secret for undeclared fixture `{name}`"),
                    )
                })?;
                let secret = self
                    .basic
                    .mint_handle(FixtureFamily::SecretStore, BTreeSet::new());
                self.secrets.handles.insert(secret.id(), material);
                Ok(Some(secret))
            }
            FixtureOutcome::Fail { error } => Err(error),
            _ => Err(secret_failure(
                FixtureErrorCode::InvalidData,
                "secretstore_lookup returned an invalid marker",
            )),
        }
    }

    pub fn secretstore_require(
        &mut self,
        store: &FixtureHandle,
        name: &str,
        source: Option<SourceLocation>,
    ) -> SecretProviderResult<FixtureHandle> {
        self.secretstore_lookup(store, name, source)?.ok_or_else(|| {
            secret_failure(
                FixtureErrorCode::NotFound,
                format!("required secret `{name}` was not granted"),
            )
        })
    }

    pub fn secret_reveal(
        &mut self,
        secret: &FixtureHandle,
        source: Option<SourceLocation>,
    ) -> SecretProviderResult<String> {
        let material = self.secret_material(secret)?;
        if material.usage != SecretUsage::Revealable {
            let message = if material.usage == SecretUsage::UseOnly {
                USE_ONLY_SECRET_REVEAL_ERROR
            } else {
                "the signing key is not revealable; use crypto.sign / crypto.public_key"
            };
            let mut call = FixtureCall::new(FixtureFamily::SecretStore, "crypto_reveal_len");
            call.source = source;
            let outcome = self.record_failure(
                call,
                FixtureErrorCode::PermissionDenied,
                message,
            );
            return outcome_redacted(outcome, "crypto_reveal_len").and_then(|_| {
                Err(secret_failure(
                    FixtureErrorCode::ProviderFailure,
                    "invalid reveal failure outcome",
                ))
            });
        }
        let revealed = String::from_utf8_lossy(&material.bytes).into_owned();
        let mut call = FixtureCall::new(FixtureFamily::SecretStore, "crypto_reveal_len");
        call.source = source;
        let outcome = if self.has_script(FixtureFamily::SecretStore) {
            self.scripted_call(call)
        } else {
            self.observe(call, redacted_outcome(material.bytes.len(), material.usage))
        };
        outcome_redacted(outcome, "crypto_reveal_len")?;
        Ok(revealed)
    }

    pub fn secret_sign(
        &mut self,
        secret: &FixtureHandle,
        message: &str,
        source: Option<SourceLocation>,
    ) -> SecretProviderResult<String> {
        self.secret_material(secret)?;
        let mut call = FixtureCall::new(FixtureFamily::SecretStore, "crypto.sign");
        call.arguments
            .insert("message".into(), FixtureValue::String(message.into()));
        call.source = source;
        outcome_public_string(self.scripted_call(call), "crypto.sign")
    }

    pub fn secret_public_key(
        &mut self,
        secret: &FixtureHandle,
        source: Option<SourceLocation>,
    ) -> SecretProviderResult<String> {
        self.secret_material(secret)?;
        let mut call = FixtureCall::new(FixtureFamily::SecretStore, "crypto.public_key");
        call.source = source;
        outcome_public_string(self.scripted_call(call), "crypto.public_key")
    }

    fn validate_store(&self, store: &FixtureHandle) -> SecretProviderResult<()> {
        if self
            .basic
            .validate_handle(store, FixtureFamily::SecretStore)
            && self.secrets.stores.contains(&store.id())
        {
            Ok(())
        } else {
            Err(secret_failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign SecretStore fixture handle",
            ))
        }
    }

    fn secret_material(
        &self,
        secret: &FixtureHandle,
    ) -> SecretProviderResult<SecretMaterial> {
        if !self
            .basic
            .validate_handle(secret, FixtureFamily::SecretStore)
        {
            return Err(secret_failure(
                FixtureErrorCode::PermissionDenied,
                "invalid or foreign Secret fixture handle",
            ));
        }
        self.secrets
            .handles
            .get(&secret.id())
            .cloned()
            .ok_or_else(|| {
                secret_failure(
                    FixtureErrorCode::PermissionDenied,
                    "invalid or foreign Secret fixture handle",
                )
            })
    }
}

fn redacted_outcome(length: usize, usage: SecretUsage) -> FixtureOutcome {
    FixtureOutcome::Return {
        value: FixtureValue::Map(BTreeMap::from([
            ("redacted".into(), FixtureValue::Bool(true)),
            ("length".into(), FixtureValue::String(length.to_string())),
            (
                "usage".into(),
                FixtureValue::String(
                    match usage {
                        SecretUsage::Revealable => "revealable",
                        SecretUsage::UseOnly => "use_only",
                        SecretUsage::Signing => "signing",
                    }
                    .into(),
                ),
            ),
        ])),
    }
}

fn outcome_redacted(outcome: FixtureOutcome, operation: &str) -> SecretProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::Map(fields),
        } if fields.get("redacted") == Some(&FixtureValue::Bool(true)) => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(secret_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid redacted result"),
        )),
    }
}

fn outcome_public_string(
    outcome: FixtureOutcome,
    operation: &str,
) -> SecretProviderResult<String> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } => Ok(value),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(secret_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid public result"),
        )),
    }
}

fn marker(
    outcome: FixtureOutcome,
    expected: &str,
    operation: &str,
) -> SecretProviderResult<()> {
    match outcome {
        FixtureOutcome::Return {
            value: FixtureValue::String(value),
        } if value == expected => Ok(()),
        FixtureOutcome::Fail { error } => Err(error),
        _ => Err(secret_failure(
            FixtureErrorCode::InvalidData,
            format!("{operation} returned an invalid {expected} marker"),
        )),
    }
}

fn secret_failure(code: FixtureErrorCode, message: impl Into<String>) -> FixtureFailure {
    FixtureFailure {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FixtureStep, SecretFixture, SecretStoreFixture, TestResult,
    };

    fn fixture(script: Vec<FixtureStep>) -> FixturePlan {
        FixturePlan {
            secrets: Some(SecretStoreFixture {
                entries: BTreeMap::from([
                    (
                        "token".into(),
                        SecretFixture {
                            hex: "746f702d736563726574".into(),
                            usage: SecretUsage::Revealable,
                        },
                    ),
                    (
                        "tls".into(),
                        SecretFixture {
                            hex: "00ff".into(),
                            usage: SecretUsage::UseOnly,
                        },
                    ),
                    (
                        "signing".into(),
                        SecretFixture {
                            hex: "11".repeat(32),
                            usage: SecretUsage::Signing,
                        },
                    ),
                ]),
                script,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn lookup_and_reveal_return_bytes_but_transcript_is_redacted() {
        let mut session = FixtureSession::new(fixture(Vec::new())).expect("session");
        let store = session.mint_fixture_secret_store(None).expect("store");
        let token = session
            .secretstore_require(&store, "token", None)
            .expect("token");
        assert_eq!(
            session.secret_reveal(&token, None).expect("reveal"),
            "top-secret"
        );
        let transcript = session.finish(TestResult::Passed);
        let rendered = serde_json::to_string(&transcript).expect("transcript");
        assert!(!rendered.contains("top-secret"));
        assert!(!rendered.contains("746f702d736563726574"));
        assert!(rendered.contains("\"redacted\""));
    }

    #[test]
    fn use_only_and_signing_secrets_cannot_be_revealed() {
        let mut session = FixtureSession::new(fixture(Vec::new())).expect("session");
        let store = session.mint_fixture_secret_store(None).expect("store");
        let tls = session
            .secretstore_require(&store, "tls", None)
            .expect("tls");
        let signing = session
            .secretstore_require(&store, "signing", None)
            .expect("signing");
        assert_eq!(
            session.secret_reveal(&tls, None).expect_err("use-only").code,
            FixtureErrorCode::PermissionDenied
        );
        assert_eq!(
            session
                .secret_reveal(&signing, None)
                .expect_err("signing")
                .code,
            FixtureErrorCode::PermissionDenied
        );
    }

    #[test]
    fn signing_uses_strict_script_without_exposing_key_material() {
        let script = vec![
            FixtureStep {
                operation: "secretstore_lookup".into(),
                target: Some("signing".into()),
                arguments: BTreeMap::new(),
                effective_rights: None,
                outcome: FixtureOutcome::Return {
                    value: FixtureValue::String("Secret".into()),
                },
                required: true,
            },
            FixtureStep {
                operation: "crypto.sign".into(),
                target: None,
                arguments: BTreeMap::from([(
                    "message".into(),
                    FixtureValue::String("payload".into()),
                )]),
                effective_rights: None,
                outcome: FixtureOutcome::Return {
                    value: FixtureValue::String("signature".into()),
                },
                required: true,
            },
        ];
        let mut session = FixtureSession::new(fixture(script)).expect("session");
        let store = session.mint_fixture_secret_store(None).expect("store");
        let signing = session
            .secretstore_require(&store, "signing", None)
            .expect("signing");
        assert_eq!(
            session
                .secret_sign(&signing, "payload", None)
                .expect("signature"),
            "signature"
        );
    }

    #[test]
    fn missing_foreign_and_invalid_signing_seed_fail_closed() {
        let plan = fixture(Vec::new());
        let mut first = FixtureSession::new(plan.clone()).expect("first");
        let mut second = FixtureSession::new(plan).expect("second");
        let store = first.mint_fixture_secret_store(None).expect("store");
        assert!(first
            .secretstore_lookup(&store, "missing", None)
            .expect("missing")
            .is_none());
        assert_eq!(
            second
                .secretstore_lookup(&store, "token", None)
                .expect_err("foreign")
                .code,
            FixtureErrorCode::PermissionDenied
        );

        let malformed = FixturePlan {
            secrets: Some(SecretStoreFixture {
                entries: BTreeMap::from([(
                    "signing".into(),
                    SecretFixture {
                        hex: "00".into(),
                        usage: SecretUsage::Signing,
                    },
                )]),
                script: Vec::new(),
            }),
            ..Default::default()
        };
        assert!(malformed.validate().is_err());
    }
}
