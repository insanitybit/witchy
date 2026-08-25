#![cfg(feature = "test-support")]

use std::process::Command;
use std::time::Instant;

use witchy_cache::{Authenticator, PlatformAuthenticator};

const CHILD_ENV: &str = "WITCHY_CACHE_PLATFORM_AUTHENTICATOR_CHILD";
const ACCOUNT_ENV: &str = "WITCHY_CACHE_PLATFORM_AUTHENTICATOR_ACCOUNT";
const MESSAGE: &[u8] = b"witchy cache cross-process credential check";

#[test]
#[ignore = "creates or reads the operating-system cache credential"]
fn platform_authenticator_is_stable_across_processes() {
    let is_child = std::env::var_os(CHILD_ENV).is_some();
    let account = std::env::var(ACCOUNT_ENV).unwrap_or_else(|_| {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).unwrap();
        format!("integration-{}-{}", std::process::id(), hex(&random))
    });
    let account: &'static str = Box::leak(account.into_boxed_str());
    let authenticator = PlatformAuthenticator::isolated_for_test(account);
    let _cleanup = (!is_child).then(|| CredentialCleanup(&authenticator));

    let first_start = Instant::now();
    let local = authenticator
        .tag(MESSAGE)
        .expect("platform authenticator should be non-interactively available");
    let first_elapsed = first_start.elapsed();
    let warm_start = Instant::now();
    let warm = authenticator.tag(MESSAGE).unwrap();
    let warm_elapsed = warm_start.elapsed();
    assert_eq!(warm, local);
    assert_ne!(local, [0_u8; 32]);
    assert_eq!(local.len(), 32);
    assert!(local.iter().any(|byte| *byte != 0));
    println!(
        "credential-timing:first_ns={},warm_ns={}",
        first_elapsed.as_nanos(),
        warm_elapsed.as_nanos()
    );

    if is_child {
        println!("credential-tag:{}", hex(&local));
        return;
    }

    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "platform_authenticator_is_stable_across_processes",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env(ACCOUNT_ENV, account)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "credential child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!("credential-tag:{}", hex(&local))),
        "child used a different platform credential: {stdout}"
    );
}

struct CredentialCleanup<'a>(&'a PlatformAuthenticator);

impl Drop for CredentialCleanup<'_> {
    fn drop(&mut self) {
        let _ = self.0.delete_isolated_test_credential();
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}
