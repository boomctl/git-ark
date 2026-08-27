use anyhow::{anyhow, Result};
use std::io::{Read, Write};

pub fn encrypt(recipient: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    let recipient: age::x25519::Recipient = recipient
        .parse()
        .map_err(|e| anyhow!("invalid age recipient: {e}"))?;
    let encryptor = age::Encryptor::with_recipients(vec![Box::new(recipient)])
        .ok_or_else(|| anyhow!("no recipients provided"))?;
    let mut out = Vec::new();
    let mut writer = encryptor.wrap_output(&mut out)?;
    writer.write_all(plaintext)?;
    writer.finish()?;
    Ok(out)
}

pub fn decrypt(identity: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let identity: age::x25519::Identity = identity
        .parse()
        .map_err(|e| anyhow!("invalid age identity: {e}"))?;
    let decryptor = match age::Decryptor::new(ciphertext)? {
        age::Decryptor::Recipients(d) => d,
        age::Decryptor::Passphrase(_) => {
            return Err(anyhow!("passphrase-encrypted payloads are not supported"))
        }
    };
    let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use age::secrecy::ExposeSecret;

    // A throwaway keypair generated in-test.
    fn keypair() -> (String, String) {
        let id = age::x25519::Identity::generate();
        let pk = id.to_public();
        (pk.to_string(), id.to_string().expose_secret().to_string())
    }

    #[test]
    fn round_trips() {
        let (pk, sk) = keypair();
        let msg = b"the whole repo bundle bytes";
        let ct = super::encrypt(&pk, msg).unwrap();
        assert_ne!(&ct[..], &msg[..]);
        let pt = super::decrypt(&sk, &ct).unwrap();
        assert_eq!(pt, msg);
    }

    #[test]
    fn wrong_key_fails() {
        let (pk, _sk) = keypair();
        let (_pk2, sk2) = keypair();
        let ct = super::encrypt(&pk, b"secret").unwrap();
        assert!(super::decrypt(&sk2, &ct).is_err());
    }
}
