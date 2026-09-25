use super::*;

#[path = "crypto_rsa_testdata.rs"]
mod vector;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn crypto_rsa_oracle_known_answer_and_tampered_inputs() {
    assert_eq!(rsa_verify_status(vector::KEY, vector::MESSAGE, vector::SIGNATURE), Ok(1));
    assert_eq!(rsa_verify_status(&vector::KEY.to_uppercase(), vector::MESSAGE, &vector::SIGNATURE.to_uppercase()), Ok(1));
    assert_eq!(rsa_verify_status(vector::KEY, "test message!", vector::SIGNATURE), Ok(0));
    assert_eq!(rsa_verify_status(vector::KEY, "test message\0", vector::SIGNATURE), Ok(0));
    assert_eq!(rsa_verify_status(vector::KEY, "test message\u{e9}", vector::SIGNATURE), Ok(0));
    let mut signature = rsa_hex(vector::SIGNATURE).unwrap();
    signature[0] ^= 1;
    assert_eq!(rsa_verify_status(vector::KEY, vector::MESSAGE, &hex(&signature)), Ok(0));
    assert_eq!(rsa_verify_status(vector::KEY, vector::MESSAGE, "00"), Ok(0));
    // Every low-valued byte can use native radix parsing's plus spelling.
    let plus = |text: &str| rsa_hex(text).unwrap().into_iter().map(|byte|
        if byte < 16 { format!("+{byte:x}") } else { format!("{byte:02x}") }).collect::<String>();
    assert_eq!(rsa_verify_status(&plus(vector::KEY), vector::MESSAGE, &plus(vector::SIGNATURE)), Ok(1));
}


