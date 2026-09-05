use arrow_mc::server::crypto::{
    CipherPair, CryptoError, ServerKey, login_digest, offline_uuid, random_uuid,
};
use openssl::{encrypt::Encrypter, pkey::PKey, rsa::Padding};

fn encrypted(key: &ServerKey, bytes: &[u8]) -> Vec<u8> {
    let public = PKey::public_key_from_der(key.public_key_der()).unwrap();
    let mut encoder = Encrypter::new(&public).unwrap();
    encoder.set_rsa_padding(Padding::PKCS1).unwrap();
    let mut output = vec![0; encoder.encrypt_len(bytes).unwrap()];
    let length = encoder.encrypt(bytes, &mut output).unwrap();
    output.truncate(length);
    output
}

#[test]
fn rsa_1024_challenge_and_key_response_fail_closed() {
    let key = ServerKey::generate().unwrap();
    assert_eq!(
        PKey::public_key_from_der(key.public_key_der())
            .unwrap()
            .bits(),
        1024
    );
    let challenge = key.challenge().unwrap();
    let secret = [0x3a; 16];
    let cipher_secret = encrypted(&key, &secret);
    let cipher_challenge = encrypted(&key, &challenge);
    let verified = key
        .verify_key_response(&cipher_secret, &cipher_challenge, challenge)
        .unwrap();
    assert_eq!(verified.shared_secret, secret);
    assert_eq!(
        verified.server_hash,
        login_digest(&secret, key.public_key_der()).unwrap()
    );
    let mut different = challenge;
    different[0] ^= 1;
    assert!(matches!(
        key.verify_key_response(&cipher_secret, &cipher_challenge, different),
        Err(CryptoError::InvalidKeyResponse)
    ));
    for invalid in [
        encrypted(&key, &[1; 15]),
        encrypted(&key, &[1; 17]),
        vec![255; 128],
        vec![0; 129],
    ] {
        assert!(matches!(
            key.verify_key_response(&invalid, &cipher_challenge, challenge),
            Err(CryptoError::InvalidKeyResponse)
        ));
    }
    assert!(matches!(
        key.verify_key_response(&cipher_secret, &[0; 128], challenge),
        Err(CryptoError::InvalidKeyResponse)
    ));
}

#[test]
fn cfb8_keeps_state_across_every_split_and_directions_independent() {
    let secret = [0x29; 16];
    let plain: Vec<_> = (0..=255u8).cycle().take(1027).collect();
    let mut reference = plain.clone();
    CipherPair::new(secret)
        .unwrap()
        .encrypt_in_place(&mut reference)
        .unwrap();
    for split in 0..=plain.len() {
        let mut pair = CipherPair::new(secret).unwrap();
        let mut encrypted = plain.clone();
        pair.encrypt_in_place(&mut encrypted[..split]).unwrap();
        pair.encrypt_in_place(&mut encrypted[split..]).unwrap();
        assert_eq!(encrypted, reference);
        // Independent inbound starts at its own IV despite outbound progress.
        pair.decrypt_in_place(&mut encrypted[..split]).unwrap();
        pair.decrypt_in_place(&mut encrypted[split..]).unwrap();
        assert_eq!(encrypted, plain);
    }
    let (mut encrypt, mut decrypt) = CipherPair::new(secret).unwrap().into_parts();
    let mut one_byte = plain.clone();
    for chunk in one_byte.chunks_mut(1) {
        encrypt.encrypt_in_place(chunk).unwrap();
    }
    assert_eq!(one_byte, reference);
    for chunk in one_byte.chunks_mut(7) {
        decrypt.decrypt_in_place(chunk).unwrap();
    }
    assert_eq!(one_byte, plain);
}

#[test]
fn uuids_use_their_explicit_online_or_offline_rules() {
    assert_eq!(
        offline_uuid("Notch").unwrap(),
        [
            0xb5, 0x0a, 0xd3, 0x85, 0x82, 0x9d, 0x31, 0x41, 0xa2, 0x16, 0x7e, 0x7d, 0x75, 0x39,
            0xba, 0x7f
        ]
    );
    assert_ne!(
        offline_uuid("Notch").unwrap(),
        offline_uuid("notch").unwrap()
    );
    let a = random_uuid().unwrap();
    let b = random_uuid().unwrap();
    assert_ne!(a, b);
    assert_eq!(a[6] >> 4, 4);
    assert_eq!(a[8] >> 6, 2);
}
