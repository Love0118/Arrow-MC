use arrow_mc::runtime::{
    AdmissionError, CpuPool, CpuPoolConfig, LOGIN_KEY_JOB_BUFFER_BYTES, LoginKeyJobError,
    LoginKeyOutput, LoginKeyTask, PacketJobError, PacketJobOutput, PacketOperation, PacketTask,
    PendingLoginKey, PendingPacket, SECTION_JOB_BUFFER_BYTES, SectionKey,
};
use arrow_mc::server::compression::{CompressionLimits, CompressionScratch, CompressionState};
use arrow_mc::server::crypto::{CipherPair, CryptoError, ServerKey};
use arrow_mc::world::section::{Registry, SectionCounts};
use openssl::{
    bn::BigNum,
    encrypt::Encrypter,
    hash::{MessageDigest, hash},
    pkey::PKey,
    rsa::Padding,
    symm::{Cipher, encrypt},
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::time::timeout;

// Deliberately public test material, never a production session secret.
const SECRET: [u8; 16] = *b"runtime-test-key";

fn pool(workers: usize, max_jobs: usize, buffer_bytes: usize) -> CpuPool {
    CpuPool::new(CpuPoolConfig {
        workers,
        max_jobs,
        buffer_bytes: buffer_bytes.max(SECTION_JOB_BUFFER_BYTES),
    })
    .unwrap()
}

fn limits() -> CompressionLimits {
    CompressionLimits {
        max_frame_body_bytes: 32_768,
        max_uncompressed_bytes: 32_768,
    }
}

fn assert_reserved(pool: &CpuPool, jobs: usize, bytes: usize) {
    let stats = pool.stats();
    assert_eq!(stats.in_flight, jobs);
    assert_eq!(stats.reserved_buffer_bytes, bytes);
}

fn packet(pool: &CpuPool, operation: PacketOperation, input: &[u8]) -> PendingPacket {
    let mut pending = pool
        .try_reserve_packet(operation, input.len(), limits())
        .unwrap();
    pending.input_mut().copy_from_slice(input);
    pending
}

fn frame(threshold: i32, payload: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut budget = usize::MAX;
    CompressionState::new(threshold)
        .encode_frame(
            payload,
            &mut CompressionScratch::default(),
            &mut output,
            limits(),
            &mut budget,
        )
        .unwrap();
    output
}

fn encrypt_stream(plaintext: &[u8]) -> Vec<u8> {
    encrypt(Cipher::aes_128_cfb8(), &SECRET, Some(&SECRET), plaintext).unwrap()
}

async fn wait_packet(task: PacketTask) -> Result<PacketJobOutput, PacketJobError> {
    timeout(Duration::from_secs(10), task.wait())
        .await
        .expect("packet worker did not complete")
}

async fn wait_login(task: LoginKeyTask) -> Result<LoginKeyOutput, LoginKeyJobError> {
    timeout(Duration::from_secs(10), task.wait())
        .await
        .expect("login-key worker did not complete")
}

fn key() -> Arc<ServerKey> {
    static KEY: OnceLock<Arc<ServerKey>> = OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::new(ServerKey::generate().unwrap())))
}

fn rsa_encrypt(key: &ServerKey, plaintext: &[u8]) -> [u8; 128] {
    let public = PKey::public_key_from_der(key.public_key_der()).unwrap();
    let mut encrypter = Encrypter::new(&public).unwrap();
    encrypter.set_rsa_padding(Padding::PKCS1).unwrap();
    let mut ciphertext = [0; 128];
    assert_eq!(encrypter.encrypt(plaintext, &mut ciphertext).unwrap(), 128);
    ciphertext
}

fn login(pool: &CpuPool, key: Arc<ServerKey>, expected: [u8; 4]) -> PendingLoginKey {
    let mut pending = pool
        .try_reserve_login_key(Arc::clone(&key), expected)
        .unwrap();
    pending
        .encrypted_secret_mut()
        .copy_from_slice(&rsa_encrypt(&key, &SECRET));
    pending
        .encrypted_challenge_mut()
        .copy_from_slice(&rsa_encrypt(&key, &expected));
    pending
}

#[tokio::test(flavor = "current_thread")]
async fn consecutive_worker_encryptions_match_one_continuous_openssl_stream() {
    for workers in [1, 4] {
        for threshold in [-1, 0, 128] {
            let pool = pool(workers, 1, 4 * SECTION_JOB_BUFFER_BYTES);
            let payloads = [vec![0x12; 31], vec![0xa5; 1024]];
            let frames = payloads.each_ref().map(|bytes| frame(threshold, bytes));
            let expected = encrypt_stream(&frames.concat());
            let (mut cipher, _) = CipherPair::new(SECRET).unwrap().into_parts();
            let mut offset = 0;

            for (payload, frame) in payloads.iter().zip(&frames) {
                let mut output = wait_packet(
                    packet(&pool, PacketOperation::Encode { threshold }, payload)
                        .submit_with_encrypt(cipher)
                        .unwrap(),
                )
                .await
                .unwrap();
                assert_eq!(output.bytes(), &expected[offset..offset + frame.len()]);
                assert!(output.take_decrypt().is_none());
                cipher = output.take_encrypt().expect("encryption state was lost");
                assert!(output.take_encrypt().is_none());
                let output_charge = if threshold < 0 || payload.len() < threshold as usize {
                    frame.len()
                } else {
                    limits().max_frame_body_bytes + 3
                };
                assert_reserved(&pool, 1, payload.len() + output_charge);
                assert!(matches!(
                    pool.try_reserve_packet(PacketOperation::Encode { threshold }, 1, limits()),
                    Err(AdmissionError::JobLimit)
                ));
                offset += frame.len();
                drop(output);
                assert_reserved(&pool, 0, 0);
            }
            assert_eq!(offset, expected.len());
            assert_eq!(pool.stats().completed_jobs, 2);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fragmented_one_two_and_three_byte_prefixes_preserve_continuous_decryption() {
    for (threshold, length, prefix_len) in
        [(-1, 31, 1), (-1, 256, 2), (-1, 16384, 3), (0, 16384, 1)]
    {
        let pool = pool(2, 1, 4 * SECTION_JOB_BUFFER_BYTES);
        let payloads = [vec![0x23; length], vec![0x7c; length + 1]];
        let frames = payloads.each_ref().map(|bytes| frame(threshold, bytes));
        let encrypted = encrypt_stream(&frames.concat());
        let (_, mut cipher) = CipherPair::new(SECRET).unwrap().into_parts();
        let mut offset = 0;

        for (payload, plaintext) in payloads.iter().zip(&frames) {
            assert_eq!(
                plaintext.iter().position(|byte| byte & 0x80 == 0).unwrap() + 1,
                prefix_len
            );
            let mut input = encrypted[offset..offset + plaintext.len()].to_vec();
            // Model a socket yielding one encrypted framing byte per read.
            for byte in input[..prefix_len].chunks_mut(1) {
                cipher.decrypt_in_place(byte).unwrap();
            }
            assert_eq!(&input[..prefix_len], &plaintext[..prefix_len]);
            let mut output = wait_packet(
                packet(&pool, PacketOperation::Decode { threshold }, &input)
                    .submit_with_decrypt(cipher, prefix_len)
                    .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(output.bytes(), payload);
            assert!(output.take_encrypt().is_none());
            cipher = output.take_decrypt().expect("decryption state was lost");
            assert!(output.take_decrypt().is_none());
            let output_charge = if threshold < 0 {
                input.len()
            } else {
                limits().max_uncompressed_bytes
            };
            assert_reserved(&pool, 1, input.len() + output_charge);
            assert!(matches!(
                pool.try_reserve_packet(PacketOperation::Decode { threshold }, 1, limits()),
                Err(AdmissionError::JobLimit)
            ));
            offset += plaintext.len();
            drop(output);
            assert_reserved(&pool, 0, 0);
        }
        assert_eq!(pool.stats().completed_jobs, 2);
    }
}

#[test]
fn incompatible_cipher_operations_and_invalid_prefixes_release_pending_leases() {
    let pool = pool(1, 1, 4 * SECTION_JOB_BUFFER_BYTES);
    let (encrypt, decrypt) = CipherPair::new(SECRET).unwrap().into_parts();
    let pending = packet(&pool, PacketOperation::Decode { threshold: -1 }, &[1, 0]);
    assert!(matches!(
        pending.submit_with_encrypt(encrypt),
        Err(AdmissionError::InvalidInput)
    ));
    assert_reserved(&pool, 0, 0);
    let pending = packet(&pool, PacketOperation::Encode { threshold: -1 }, &[0]);
    assert!(matches!(
        pending.submit_with_decrypt(decrypt, 0),
        Err(AdmissionError::InvalidInput)
    ));
    assert_reserved(&pool, 0, 0);
    for (input, prefix_len) in [(&[1, 0][..], 3), (&[1, 0, 1, 0][..], 4)] {
        let (_, decrypt) = CipherPair::new(SECRET).unwrap().into_parts();
        let pending = packet(&pool, PacketOperation::Decode { threshold: -1 }, input);
        assert!(matches!(
            pending.submit_with_decrypt(decrypt, prefix_len),
            Err(AdmissionError::InvalidInput)
        ));
        assert_reserved(&pool, 0, 0);
    }
    assert_eq!(pool.stats().completed_jobs, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn encrypted_codec_failures_return_no_state_and_release_the_job_budget() {
    let pool = pool(1, 1, 4 * SECTION_JOB_BUFFER_BYTES);
    for (plaintext, trailing) in [(&[2, 0][..], false), (&[1, 0, 1, 0][..], true)] {
        let encrypted = encrypt_stream(plaintext);
        let (_, decrypt) = CipherPair::new(SECRET).unwrap().into_parts();
        let result = wait_packet(
            packet(&pool, PacketOperation::Decode { threshold: -1 }, &encrypted)
                .submit_with_decrypt(decrypt, 0)
                .unwrap(),
        )
        .await;
        if trailing {
            assert!(matches!(result, Err(PacketJobError::TrailingFrameBytes)));
        } else {
            assert!(matches!(result, Err(PacketJobError::Codec(_))));
        }
        assert_reserved(&pool, 0, 0);
    }
    let mut pending = pool
        .try_reserve_packet(
            PacketOperation::Encode { threshold: 0 },
            64,
            CompressionLimits {
                max_frame_body_bytes: 1,
                max_uncompressed_bytes: 64,
            },
        )
        .unwrap();
    pending.input_mut().fill(0x5a);
    let (encrypt, _) = CipherPair::new(SECRET).unwrap().into_parts();
    assert!(matches!(
        wait_packet(pending.submit_with_encrypt(encrypt).unwrap()).await,
        Err(PacketJobError::Codec(_))
    ));
    assert_reserved(&pool, 0, 0);

    let (encrypt, _) = CipherPair::new(SECRET).unwrap().into_parts();
    let output = wait_packet(
        packet(&pool, PacketOperation::Encode { threshold: -1 }, &[0x42])
            .submit_with_encrypt(encrypt)
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(output.bytes(), encrypt_stream(&[1, 0x42]));
    drop(output);
    assert_reserved(&pool, 0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn verified_login_secret_and_signed_hash_retain_their_full_reservation() {
    let key = key();
    let challenge = key.challenge().unwrap();
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    assert_eq!(LOGIN_KEY_JOB_BUFFER_BYTES, 297);
    let output = wait_login(login(&pool, Arc::clone(&key), challenge).submit().unwrap())
        .await
        .unwrap();
    assert!(output.secret().shared_secret == SECRET);

    // Big-number arithmetic independently checks Java's signed SHA1 integer.
    let digest = hash(
        MessageDigest::sha1(),
        &[SECRET.as_slice(), key.public_key_der()].concat(),
    )
    .unwrap();
    let mut expected = BigNum::from_slice(&digest).unwrap();
    if digest[0] & 0x80 != 0 {
        let mut modulus = BigNum::new().unwrap();
        modulus.set_bit(160).unwrap();
        let mut magnitude = BigNum::new().unwrap();
        magnitude.checked_sub(&modulus, &expected).unwrap();
        magnitude.set_negative(true);
        expected = magnitude;
    }
    let actual = BigNum::from_hex_str(&output.secret().server_hash).unwrap();
    assert!(actual == expected);
    let digits = output.secret().server_hash.trim_start_matches('-');
    assert!(!digits.is_empty());
    assert!(digits == "0" || !digits.starts_with('0'));
    assert!(
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_reserved(&pool, 1, LOGIN_KEY_JOB_BUFFER_BYTES);
    assert!(matches!(
        pool.try_reserve_login_key(Arc::clone(&key), challenge),
        Err(AdmissionError::JobLimit)
    ));
    drop(output);
    assert_reserved(&pool, 0, 0);
    let replacement = pool.try_reserve_login_key(key, challenge).unwrap();
    drop(replacement);
    assert_reserved(&pool, 0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_rsa_ciphertexts_challenges_and_secret_lengths_share_one_public_error() {
    let key = key();
    let expected = [1, 2, 3, 4];
    let pool = pool(1, 1, SECTION_JOB_BUFFER_BYTES);
    for (secret, challenge) in [
        (&SECRET[..], &[1, 2, 3, 5][..]),
        (&SECRET[..], &[1, 2, 3][..]),
        (&SECRET[..], &[1, 2, 3, 4, 5][..]),
        (&SECRET[..0], &expected[..]),
        (&SECRET[..15], &expected[..]),
        (&[0x5a; 17][..], &expected[..]),
    ] {
        let mut pending = pool
            .try_reserve_login_key(Arc::clone(&key), expected)
            .unwrap();
        pending
            .encrypted_secret_mut()
            .copy_from_slice(&rsa_encrypt(&key, secret));
        pending
            .encrypted_challenge_mut()
            .copy_from_slice(&rsa_encrypt(&key, challenge));
        assert!(matches!(
            wait_login(pending.submit().unwrap()).await,
            Err(LoginKeyJobError::Crypto(CryptoError::InvalidKeyResponse))
        ));
        assert_reserved(&pool, 0, 0);
    }
    for corrupt_secret in [false, true] {
        let mut pending = login(&pool, Arc::clone(&key), expected);
        // This is larger than the modulus. Bad padding alone uses OpenSSL's
        // implicit rejection and can return a synthetic 16-byte secret.
        if corrupt_secret {
            pending.encrypted_secret_mut().fill(0xff);
        } else {
            pending.encrypted_challenge_mut().fill(0xff);
        }
        assert!(matches!(
            wait_login(pending.submit().unwrap()).await,
            Err(LoginKeyJobError::Crypto(CryptoError::InvalidKeyResponse))
        ));
        assert_reserved(&pool, 0, 0);
    }
    let valid = wait_login(login(&pool, key, expected).submit().unwrap())
        .await
        .unwrap();
    assert!(valid.secret().shared_secret == SECRET);
    drop(valid);
    assert_reserved(&pool, 0, 0);
    assert_eq!(pool.stats().completed_jobs, 9);
}

#[tokio::test(flavor = "current_thread")]
async fn sections_encrypted_packets_and_rsa_share_slots_bytes_and_retained_outputs() {
    let key = key();
    let packet_charge = 3;
    let total = SECTION_JOB_BUFFER_BYTES + packet_charge + LOGIN_KEY_JOB_BUFFER_BYTES;
    for max_jobs in [3, 4] {
        let budget = total - usize::from(max_jobs == 4);
        let pool = pool(2, max_jobs, budget);
        let section = pool
            .try_reserve_section(
                SectionKey {
                    world_epoch: 1,
                    chunk_x: -1,
                    chunk_z: 2,
                    section_y: -4,
                    revision: 3,
                },
                Registry::new(1).unwrap(),
                Registry::new(1).unwrap(),
                SectionCounts {
                    non_empty_blocks: 0,
                    fluid_blocks: 0,
                },
            )
            .unwrap();
        let pending_packet = packet(&pool, PacketOperation::Encode { threshold: -1 }, &[0]);
        assert_reserved(&pool, 2, SECTION_JOB_BUFFER_BYTES + packet_charge);
        if max_jobs == 4 {
            assert!(matches!(
                pool.try_reserve_login_key(Arc::clone(&key), [1; 4]),
                Err(AdmissionError::ByteLimit)
            ));
            assert_reserved(&pool, 2, SECTION_JOB_BUFFER_BYTES + packet_charge);
            drop(pending_packet);
            let pending_login = login(&pool, Arc::clone(&key), [1; 4]);
            assert_reserved(
                &pool,
                2,
                SECTION_JOB_BUFFER_BYTES + LOGIN_KEY_JOB_BUFFER_BYTES,
            );
            drop(pending_login);
            drop(section);
            assert_reserved(&pool, 0, 0);
            continue;
        }
        let pending_login = login(&pool, Arc::clone(&key), [1; 4]);
        assert_reserved(&pool, 3, total);
        assert!(matches!(
            pool.try_reserve_login_key(Arc::clone(&key), [1; 4]),
            Err(AdmissionError::JobLimit)
        ));
        assert!(matches!(
            pool.try_reserve_packet(PacketOperation::Encode { threshold: -1 }, 0, limits()),
            Err(AdmissionError::JobLimit)
        ));
        let section_task = section.submit().unwrap();
        let (encrypt, _) = CipherPair::new(SECRET).unwrap().into_parts();
        let packet_task = pending_packet.submit_with_encrypt(encrypt).unwrap();
        let login_task = pending_login.submit().unwrap();
        let mut packet_output = wait_packet(packet_task).await.unwrap();
        let login_output = wait_login(login_task).await.unwrap();
        let section_output = section_task.wait().unwrap();
        assert!(section_output.bytes().is_ok());
        assert_eq!(packet_output.bytes(), encrypt_stream(&[1, 0]));
        assert!(login_output.secret().shared_secret == SECRET);
        let cipher = packet_output.take_encrypt().unwrap();
        assert_reserved(&pool, 3, total);
        drop(cipher);
        assert_reserved(&pool, 3, total);
        drop(packet_output);
        assert_reserved(
            &pool,
            2,
            SECTION_JOB_BUFFER_BYTES + LOGIN_KEY_JOB_BUFFER_BYTES,
        );
        drop(section_output);
        assert_reserved(&pool, 1, LOGIN_KEY_JOB_BUFFER_BYTES);
        drop(login_output);
        assert_reserved(&pool, 0, 0);
        assert_eq!(pool.stats().completed_jobs, 3);
        assert_eq!(pool.stats().peak_reserved_buffer_bytes, total);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_and_dropped_login_receivers_release_before_a_later_worker_completion() {
    let key = key();
    let pool = pool(1, 9, SECTION_JOB_BUFFER_BYTES);
    for index in 0..8 {
        let mut task = login(&pool, Arc::clone(&key), [index; 4]).submit().unwrap();
        if index % 2 == 0 {
            task.cancel();
            assert!(matches!(
                wait_login(task).await,
                Err(LoginKeyJobError::Cancelled)
            ));
        } else {
            drop(task);
        }
    }
    // With one FIFO worker this result is also a barrier for prior cleanup.
    let output = wait_login(login(&pool, key, [9; 4]).submit().unwrap())
        .await
        .unwrap();
    assert!(output.secret().shared_secret == SECRET);
    assert_reserved(&pool, 1, LOGIN_KEY_JOB_BUFFER_BYTES);
    drop(output);
    assert_reserved(&pool, 0, 0);
    assert_eq!(pool.stats().completed_jobs, 9);
}

#[test]
fn closed_pool_rejects_cipher_and_login_submissions_without_retaining_reservations() {
    let key = key();
    let pool = pool(1, 3, 4 * SECTION_JOB_BUFFER_BYTES);
    let pending_encrypt = packet(&pool, PacketOperation::Encode { threshold: -1 }, &[0]);
    let pending_decrypt = packet(&pool, PacketOperation::Decode { threshold: -1 }, &[1, 0]);
    let pending_login = login(&pool, Arc::clone(&key), [1; 4]);
    let (encrypt, decrypt) = CipherPair::new(SECRET).unwrap().into_parts();
    pool.close();
    assert!(matches!(
        pending_encrypt.submit_with_encrypt(encrypt),
        Err(AdmissionError::Closed)
    ));
    assert!(matches!(
        pending_decrypt.submit_with_decrypt(decrypt, 0),
        Err(AdmissionError::Closed)
    ));
    assert!(matches!(
        pending_login.submit(),
        Err(AdmissionError::Closed)
    ));
    assert!(matches!(
        pool.try_reserve_login_key(key, [1; 4]),
        Err(AdmissionError::Closed)
    ));
    assert_reserved(&pool, 0, 0);
    assert_eq!(pool.stats().completed_jobs, 0);
    pool.shutdown().unwrap();
}
