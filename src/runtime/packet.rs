//! Concrete packet-codec jobs sharing the section pool's queue and admission.

use super::{AdmissionError, CpuPool, Job, Lease, Shared, finish_job, lock};
use crate::server::compression::{
    CompressionError, CompressionLimits, CompressionScratch, CompressionState,
    MAX_FRAME_BODY_BYTES, MAX_UNCOMPRESSED_BYTES,
};
use crate::server::crypto::{CryptoError, DecryptCipher, EncryptCipher, LoginSecret, ServerKey};
use crate::wire::varint_len;
use std::{fmt, sync::Arc};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug)]
pub enum PacketOperation {
    Encode { threshold: i32 },
    Decode { threshold: i32 },
}

#[derive(Debug)]
pub enum PacketJobError {
    Cancelled,
    Codec(CompressionError),
    Crypto(CryptoError),
    TrailingFrameBytes,
    WorkerPanicked,
}

impl fmt::Display for PacketJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("packet CPU job cancelled"),
            Self::Codec(error) => write!(f, "packet CPU codec failed: {error}"),
            Self::Crypto(error) => write!(f, "packet CPU cipher failed: {error}"),
            Self::TrailingFrameBytes => f.write_str("packet job contains more than one frame"),
            Self::WorkerPanicked => f.write_str("packet CPU worker panicked"),
        }
    }
}
impl std::error::Error for PacketJobError {}

/// The full input and conservative maximum output are allocated only after the
/// global slot/byte lease. Buffer fields precede the lease for destruction order.
pub struct PendingPacket {
    input: Vec<u8>,
    output: Vec<u8>,
    operation: PacketOperation,
    limits: CompressionLimits,
    encrypt: Option<EncryptCipher>,
    decrypt: Option<DecryptCipher>,
    plaintext_prefix_len: usize,
    lease: Lease,
}

pub(super) struct PacketJob {
    pending: PendingPacket,
    sender: oneshot::Sender<Result<PacketJobOutput, PacketJobError>>,
    #[cfg(test)]
    gate: Option<Arc<super::TestGate>>,
}

pub struct PacketTask {
    receiver: Option<oneshot::Receiver<Result<PacketJobOutput, PacketJobError>>>,
    cancelled: bool,
}

/// Borrow through socket write or parsing; the allocation cannot be detached
/// from its reservation. Ready output does not occupy a worker thread.
pub struct PacketJobOutput {
    bytes: Vec<u8>,
    encrypt: Option<EncryptCipher>,
    decrypt: Option<DecryptCipher>,
    _lease: Lease,
}

impl CpuPool {
    pub fn try_reserve_packet(
        &self,
        operation: PacketOperation,
        input_len: usize,
        limits: CompressionLimits,
    ) -> Result<PendingPacket, AdmissionError> {
        if limits.max_frame_body_bytes == 0
            || limits.max_frame_body_bytes > MAX_FRAME_BODY_BYTES
            || limits.max_uncompressed_bytes > MAX_UNCOMPRESSED_BYTES
        {
            return Err(AdmissionError::InvalidInput);
        }
        let (input_max, output_max) = match operation {
            PacketOperation::Encode { threshold } => {
                let input_max = if threshold < 0 {
                    limits.max_frame_body_bytes
                } else {
                    limits.max_uncompressed_bytes
                };
                if input_len > input_max {
                    return Err(AdmissionError::InvalidInput);
                }
                let output_max = if threshold < 0 || input_len < threshold as usize {
                    let body = input_len + usize::from(threshold >= 0);
                    (body + varint_len(body as i32)).min(limits.max_frame_body_bytes + 3)
                } else {
                    limits.max_frame_body_bytes + 3
                };
                (input_max, output_max)
            }
            PacketOperation::Decode { threshold } => (
                limits.max_frame_body_bytes + 3,
                if threshold < 0 {
                    input_len
                } else {
                    limits
                        .max_uncompressed_bytes
                        .max(limits.max_frame_body_bytes)
                },
            ),
        };
        if input_len > input_max {
            return Err(AdmissionError::InvalidInput);
        }
        let bytes = input_len
            .checked_add(output_max)
            .ok_or(AdmissionError::ByteLimit)?;
        let lease = self.try_reserve(bytes)?;
        let mut input = Vec::new();
        input
            .try_reserve_exact(input_len)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        if input.capacity() > input_len {
            return Err(AdmissionError::ByteLimit);
        }
        input.resize(input_len, 0);
        let mut output = Vec::new();
        output
            .try_reserve_exact(output_max)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        if output.capacity() > output_max {
            return Err(AdmissionError::ByteLimit);
        }
        Ok(PendingPacket {
            input,
            output,
            operation,
            limits,
            encrypt: None,
            decrypt: None,
            plaintext_prefix_len: 0,
            lease,
        })
    }
}

impl PendingPacket {
    pub fn input_mut(&mut self) -> &mut [u8] {
        &mut self.input
    }

    pub fn submit_with_encrypt(
        mut self,
        cipher: EncryptCipher,
    ) -> Result<PacketTask, AdmissionError> {
        if !matches!(self.operation, PacketOperation::Encode { .. }) {
            return Err(AdmissionError::InvalidInput);
        }
        self.encrypt = Some(cipher);
        self.submit()
    }

    /// The connection has already decrypted at most three framing bytes. The
    /// body is decrypted by this worker using the next position in that stream.
    /// On every error/cancel the moved cipher is destroyed; close the socket.
    pub fn submit_with_decrypt(
        mut self,
        cipher: DecryptCipher,
        plaintext_prefix_len: usize,
    ) -> Result<PacketTask, AdmissionError> {
        if !matches!(self.operation, PacketOperation::Decode { .. })
            || plaintext_prefix_len > 3
            || plaintext_prefix_len > self.input.len()
        {
            return Err(AdmissionError::InvalidInput);
        }
        self.decrypt = Some(cipher);
        self.plaintext_prefix_len = plaintext_prefix_len;
        self.submit()
    }

    pub fn submit(self) -> Result<PacketTask, AdmissionError> {
        self.enqueue(
            #[cfg(test)]
            None,
        )
    }

    fn enqueue(
        self,
        #[cfg(test)] gate: Option<Arc<super::TestGate>>,
    ) -> Result<PacketTask, AdmissionError> {
        let (sender, receiver) = oneshot::channel();
        let shared = Arc::clone(&self.lease.shared);
        {
            let mut state = lock(&shared.state);
            if state.closed {
                return Err(AdmissionError::Closed);
            }
            debug_assert!(state.queue.len() < shared.config.max_jobs);
            state.queue.push_back(Job::Packet(PacketJob {
                pending: self,
                sender,
                #[cfg(test)]
                gate,
            }));
            state.stats.queued += 1;
        }
        shared.work.notify_one();
        Ok(PacketTask {
            receiver: Some(receiver),
            cancelled: false,
        })
    }
}

impl PacketTask {
    /// Dropping this future closes the receiver. Queued/running jobs retain
    /// their lease until the worker actually drops input and output buffers.
    pub async fn wait(mut self) -> Result<PacketJobOutput, PacketJobError> {
        self.wait_mut().await
    }

    /// A timer may cancel this borrowed wait without dropping the receiver or
    /// its reserved result. Call again to continue the same job. After one
    /// completed wait, further waits return Cancelled.
    pub async fn wait_mut(&mut self) -> Result<PacketJobOutput, PacketJobError> {
        let receiver = self.receiver.as_mut().ok_or(PacketJobError::Cancelled)?;
        let result = receiver.await.unwrap_or(Err(PacketJobError::Cancelled));
        self.receiver = None;
        if self.cancelled {
            drop(result);
            Err(PacketJobError::Cancelled)
        } else {
            result
        }
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(receiver) = &mut self.receiver {
            receiver.close();
        }
    }
}

impl PacketJobOutput {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn take_encrypt(&mut self) -> Option<EncryptCipher> {
        self.encrypt.take()
    }
    pub fn take_decrypt(&mut self) -> Option<DecryptCipher> {
        self.decrypt.take()
    }
}

pub(super) fn run(job: PacketJob, scratch: &mut Option<CompressionScratch>, shared: &Shared) {
    #[cfg(test)]
    if let Some(gate) = &job.gate {
        gate.block();
    }
    let PacketJob {
        pending, sender, ..
    } = job;
    if sender.is_closed() {
        drop(pending);
        finish_job(shared);
        return;
    }
    let PendingPacket {
        mut input,
        mut output,
        operation,
        limits,
        mut encrypt,
        mut decrypt,
        plaintext_prefix_len,
        lease,
    } = pending;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(cipher) = &mut decrypt {
            cipher
                .decrypt_in_place(&mut input[plaintext_prefix_len..])
                .map_err(PacketJobError::Crypto)?;
        }
        let scratch = scratch.get_or_insert_with(CompressionScratch::default);
        let mut no_allocation = 0;
        match operation {
            PacketOperation::Encode { threshold } => {
                CompressionState::new(threshold)
                    .encode_frame(&input, scratch, &mut output, limits, &mut no_allocation)
                    .map_err(PacketJobError::Codec)?;
                if let Some(cipher) = &mut encrypt {
                    cipher
                        .encrypt_in_place(&mut output)
                        .map_err(PacketJobError::Crypto)?;
                }
                Ok(())
            }
            PacketOperation::Decode { threshold } => {
                let mut cursor = input.as_slice();
                CompressionState::new(threshold)
                    .decode_frame(
                        &mut cursor,
                        scratch,
                        &mut output,
                        limits,
                        &mut no_allocation,
                    )
                    .map_err(PacketJobError::Codec)?;
                if cursor.is_empty() {
                    Ok(())
                } else {
                    Err(PacketJobError::TrailingFrameBytes)
                }
            }
        }
    }))
    .unwrap_or(Err(PacketJobError::WorkerPanicked));
    drop(input);
    let completion = match result {
        Ok(()) if !sender.is_closed() => Ok(PacketJobOutput {
            bytes: output,
            encrypt,
            decrypt,
            _lease: lease,
        }),
        other => {
            drop(output);
            drop(encrypt);
            drop(decrypt);
            drop(lease);
            Err(other.err().unwrap_or(PacketJobError::Cancelled))
        }
    };
    finish_job(shared);
    // A closed receiver drops the completion here, including its lease.
    let _ = sender.send(completion);
}

/// Two fixed 1024-bit RSA ciphertexts and at most 41 ASCII hash bytes. Shared
/// key/native OpenSSL workspace and fixed worker stacks are separate overhead,
/// bounded by key size and worker count, not an exact native allocator budget.
pub const LOGIN_KEY_JOB_BUFFER_BYTES: usize = 128 * 2 + 41;

pub struct PendingLoginKey {
    input: Vec<u8>,
    key: Arc<ServerKey>,
    expected: [u8; 4],
    lease: Lease,
}

pub(super) struct LoginKeyJob {
    pending: PendingLoginKey,
    sender: oneshot::Sender<Result<LoginKeyOutput, LoginKeyJobError>>,
}

pub struct LoginKeyTask {
    receiver: oneshot::Receiver<Result<LoginKeyOutput, LoginKeyJobError>>,
    cancelled: bool,
}

pub struct LoginKeyOutput {
    secret: LoginSecret,
    _lease: Lease,
}

#[derive(Debug)]
pub enum LoginKeyJobError {
    Cancelled,
    Crypto(CryptoError),
    WorkerPanicked,
}

impl fmt::Display for LoginKeyJobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("login key verification cancelled"),
            Self::Crypto(error) => write!(f, "login key verification failed: {error}"),
            Self::WorkerPanicked => f.write_str("login key verification worker panicked"),
        }
    }
}
impl std::error::Error for LoginKeyJobError {}

impl CpuPool {
    pub fn try_reserve_login_key(
        &self,
        key: Arc<ServerKey>,
        expected: [u8; 4],
    ) -> Result<PendingLoginKey, AdmissionError> {
        let lease = self.try_reserve(LOGIN_KEY_JOB_BUFFER_BYTES)?;
        let mut input = Vec::new();
        input
            .try_reserve_exact(256)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        if input.capacity() > 256 {
            return Err(AdmissionError::ByteLimit);
        }
        input.resize(256, 0);
        Ok(PendingLoginKey {
            input,
            key,
            expected,
            lease,
        })
    }
}

impl PendingLoginKey {
    pub fn encrypted_secret_mut(&mut self) -> &mut [u8; 128] {
        (&mut self.input[..128]).try_into().unwrap()
    }
    pub fn encrypted_challenge_mut(&mut self) -> &mut [u8; 128] {
        (&mut self.input[128..]).try_into().unwrap()
    }
    pub fn submit(self) -> Result<LoginKeyTask, AdmissionError> {
        let (sender, receiver) = oneshot::channel();
        let shared = Arc::clone(&self.lease.shared);
        {
            let mut state = lock(&shared.state);
            if state.closed {
                return Err(AdmissionError::Closed);
            }
            debug_assert!(state.queue.len() < shared.config.max_jobs);
            state.queue.push_back(Job::VerifyLoginKey(LoginKeyJob {
                pending: self,
                sender,
            }));
            state.stats.queued += 1;
        }
        shared.work.notify_one();
        Ok(LoginKeyTask {
            receiver,
            cancelled: false,
        })
    }
}

impl LoginKeyTask {
    pub async fn wait(self) -> Result<LoginKeyOutput, LoginKeyJobError> {
        let result = self
            .receiver
            .await
            .unwrap_or(Err(LoginKeyJobError::Cancelled));
        if self.cancelled {
            drop(result);
            Err(LoginKeyJobError::Cancelled)
        } else {
            result
        }
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.receiver.close();
    }
}
impl LoginKeyOutput {
    pub fn secret(&self) -> &LoginSecret {
        &self.secret
    }
}

pub(super) fn verify_login_key(job: LoginKeyJob, shared: &Shared) {
    let LoginKeyJob { pending, sender } = job;
    if sender.is_closed() {
        drop(pending);
        finish_job(shared);
        return;
    }
    let PendingLoginKey {
        input,
        key,
        expected,
        lease,
    } = pending;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        key.verify_key_response(&input[..128], &input[128..], expected)
    }))
    .map_err(|_| LoginKeyJobError::WorkerPanicked)
    .and_then(|result| result.map_err(LoginKeyJobError::Crypto));
    drop(input);
    drop(key);
    let completion = match result {
        Ok(secret) if !sender.is_closed() => Ok(LoginKeyOutput {
            secret,
            _lease: lease,
        }),
        Ok(secret) => {
            drop(secret);
            drop(lease);
            Err(LoginKeyJobError::Cancelled)
        }
        Err(error) => {
            drop(lease);
            Err(error)
        }
    };
    finish_job(shared);
    let _ = sender.send(completion);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{CpuPoolConfig, SECTION_JOB_BUFFER_BYTES, TestGate};
    use std::sync::{Condvar, Mutex, mpsc};
    use std::time::Duration;

    struct Release(Arc<TestGate>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    fn setup() -> (CpuPool, Arc<TestGate>, mpsc::Receiver<()>, Release) {
        let pool = CpuPool::new(CpuPoolConfig {
            workers: 1,
            max_jobs: 1,
            buffer_bytes: SECTION_JOB_BUFFER_BYTES,
        })
        .unwrap();
        let (started, receive) = mpsc::sync_channel(1);
        let gate = Arc::new(TestGate {
            started,
            released: Mutex::new(false),
            changed: Condvar::new(),
        });
        (pool, Arc::clone(&gate), receive, Release(gate))
    }

    fn pending(pool: &CpuPool) -> PendingPacket {
        let mut packet = pool
            .try_reserve_packet(
                PacketOperation::Encode { threshold: -1 },
                2,
                CompressionLimits {
                    max_frame_body_bytes: 100,
                    max_uncompressed_bytes: 200,
                },
            )
            .unwrap();
        packet.input_mut().copy_from_slice(&[0, 17]);
        packet
    }

    #[tokio::test]
    async fn cancelling_running_packet_keeps_memory_until_worker_exits() {
        let (pool, gate, started, _release) = setup();
        let task = pending(&pool).enqueue(Some(Arc::clone(&gate))).unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut future = Box::pin(task.wait());
        assert!(
            tokio::time::timeout(Duration::from_millis(1), &mut future)
                .await
                .is_err()
        );
        drop(future);
        assert_eq!(pool.stats().in_flight, 1);
        assert_eq!(pool.stats().reserved_buffer_bytes, 5);
        gate.release();
        pool.close();
        let shared = Arc::clone(&pool.shared);
        pool.shutdown().unwrap();
        assert_eq!(lock(&shared.state).stats.in_flight, 0);
        assert_eq!(lock(&shared.state).stats.reserved_buffer_bytes, 0);
    }

    #[tokio::test]
    async fn cancelled_borrowed_wait_resumes_same_packet_without_releasing_lease() {
        let (pool, gate, started, _release) = setup();
        let mut task = pending(&pool).enqueue(Some(Arc::clone(&gate))).unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), task.wait_mut())
                .await
                .is_err()
        );
        assert_eq!(pool.stats().reserved_buffer_bytes, 5);
        gate.release();
        let output = task.wait_mut().await.unwrap();
        assert_eq!(output.bytes(), &[2, 0, 17]);
        assert_eq!(pool.stats().running, 0);
        assert_eq!(pool.stats().in_flight, 1);
        assert!(matches!(
            task.wait_mut().await,
            Err(PacketJobError::Cancelled)
        ));
        drop(output);
        assert_eq!(pool.stats().in_flight, 0);
    }
}
