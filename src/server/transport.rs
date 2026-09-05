//! One socket owner for ordered encrypted/compressed packet I/O.
//!
//! Frame headers are at most three bytes and decrypt inline. Admitted CPU jobs
//! own the cipher state for larger bodies, the codec scratch and leased packet
//! buffers. Read deadlines retain partial progress; cancellation of a write or
//! dropping the transport closes its socket. Running worker leases remain live.

use super::{
    compression::{CompressionError, CompressionLimits, CompressionState},
    crypto::{CipherPair, CryptoError, DecryptCipher, EncryptCipher},
};
use crate::runtime::{
    AdmissionError, CpuPool, PacketJobError, PacketJobOutput, PacketOperation, PacketTask,
    PendingPacket,
};
use std::{fmt, io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Instant, timeout, timeout_at},
};

#[derive(Clone, Copy, Debug)]
pub struct TransportLimits {
    pub max_frame_body_bytes: usize,
    pub max_uncompressed_bytes: usize,
    pub read_idle_timeout: Duration,
    pub write_timeout: Duration,
}

impl Default for TransportLimits {
    fn default() -> Self {
        let codec = CompressionLimits::default();
        Self {
            max_frame_body_bytes: codec.max_frame_body_bytes,
            max_uncompressed_bytes: codec.max_uncompressed_bytes,
            read_idle_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
pub enum TransportError {
    Closed,
    FrameLength,
    Timeout,
    Admission(AdmissionError),
    Worker(PacketJobError),
    Compression(CompressionError),
    Crypto(CryptoError),
    Io(io::Error),
}

impl fmt::Display for TransportError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => output.write_str("connection transport is closed"),
            Self::FrameLength => output.write_str("invalid connection frame length"),
            Self::Timeout => output.write_str("connection I/O deadline exceeded"),
            Self::Admission(error) => write!(output, "packet admission failed: {error}"),
            Self::Worker(error) => write!(output, "packet CPU job failed: {error}"),
            Self::Compression(error) => write!(output, "compression state failed: {error}"),
            Self::Crypto(error) => write!(output, "connection cipher failed: {error}"),
            Self::Io(error) => write!(output, "connection I/O failed: {error}"),
        }
    }
}

impl std::error::Error for TransportError {}

pub struct ConnectionTransport {
    stream: Option<TcpStream>,
    cpu: Arc<CpuPool>,
    compression: CompressionState,
    encryptor: Option<EncryptCipher>,
    decryptor: Option<DecryptCipher>,
    limits: TransportLimits,
    reading: ReadProgress,
    last_read: Instant,
}

impl ConnectionTransport {
    pub fn new(stream: TcpStream, cpu: Arc<CpuPool>, limits: TransportLimits) -> Self {
        Self {
            stream: Some(stream),
            cpu,
            compression: CompressionState::new(-1),
            encryptor: None,
            decryptor: None,
            limits,
            reading: ReadProgress::default(),
            last_read: Instant::now(),
        }
    }

    pub fn close(&mut self) {
        self.stream = None;
        self.encryptor = None;
        self.decryptor = None;
        self.reading = ReadProgress::default();
    }

    pub fn is_open(&self) -> bool {
        self.stream.is_some()
    }

    pub fn compression_threshold(&self) -> Result<Option<i32>, TransportError> {
        self.compression
            .threshold()
            .map_err(TransportError::Compression)
    }

    /// Enable after the last unencrypted Key frame has been consumed. This
    /// transport reads exactly one frame without buffering following ciphertext.
    pub fn enable_encryption(&mut self, shared_secret: [u8; 16]) -> Result<(), TransportError> {
        if self.stream.is_none()
            || self.encryptor.is_some()
            || self.decryptor.is_some()
            || !self.reading.is_idle()
        {
            return Err(TransportError::Closed);
        }
        let pair = CipherPair::new(shared_secret).map_err(TransportError::Crypto)?;
        let (encryptor, decryptor) = pair.into_parts();
        self.encryptor = Some(encryptor);
        self.decryptor = Some(decryptor);
        Ok(())
    }

    fn codec_limits(&self) -> CompressionLimits {
        CompressionLimits {
            max_frame_body_bytes: self.limits.max_frame_body_bytes,
            max_uncompressed_bytes: self.limits.max_uncompressed_bytes,
        }
    }

    /// The returned buffer retains its global byte/job lease. Reads are cancel
    /// safe: partial bytes, cipher position and any CPU job remain in self.
    pub async fn read_packet(&mut self) -> Result<PacketJobOutput, TransportError> {
        self.read_packet_inner(None)
            .await?
            .ok_or(TransportError::Timeout)
    }

    /// Returns None at a scheduling deadline without abandoning partial input.
    /// This lets configuration send keepalives while a fragmented inbound frame
    /// or admitted CPU job is pending. The independent read-idle deadline is fatal.
    pub async fn read_packet_until(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<PacketJobOutput>, TransportError> {
        self.read_packet_inner(Some(deadline)).await
    }

    async fn read_packet_inner(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<PacketJobOutput>, TransportError> {
        let result = self.read_progress(deadline).await;
        if result.is_err() {
            self.close();
        }
        result
    }

    async fn read_progress(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<Option<PacketJobOutput>, TransportError> {
        loop {
            if self.stream.is_none() {
                return Err(TransportError::Closed);
            }
            if let Some(task) = self.reading.task.as_mut() {
                let idle_deadline = self.last_read + self.limits.read_idle_timeout;
                let until = deadline.map_or(idle_deadline, |deadline| deadline.min(idle_deadline));
                let result = match timeout_at(until, task.wait_mut()).await {
                    Ok(result) => result,
                    Err(_) if until == idle_deadline => return Err(TransportError::Timeout),
                    Err(_) => return Ok(None),
                }
                .map_err(TransportError::Worker)?;
                let mut result = result;
                self.decryptor = result.take_decrypt();
                self.reading = ReadProgress::default();
                return Ok(Some(result));
            }
            let idle_deadline = self.last_read + self.limits.read_idle_timeout;
            let until = deadline.map_or(idle_deadline, |deadline| deadline.min(idle_deadline));
            if Instant::now() >= until {
                return if Instant::now() >= idle_deadline {
                    Err(TransportError::Timeout)
                } else {
                    Ok(None)
                };
            }
            if !self.reading.header_complete {
                let index = self.reading.prefix_length;
                let read = timeout_at(
                    until,
                    self.stream
                        .as_mut()
                        .unwrap()
                        .read(&mut self.reading.prefix[index..index + 1]),
                )
                .await;
                let count = match read {
                    Ok(result) => result.map_err(TransportError::Io)?,
                    Err(_) if until == idle_deadline => return Err(TransportError::Timeout),
                    Err(_) => return Ok(None),
                };
                if count == 0 {
                    return Err(TransportError::Io(io::Error::from(
                        io::ErrorKind::UnexpectedEof,
                    )));
                }
                self.last_read = Instant::now();
                if let Some(cipher) = self.decryptor.as_mut() {
                    cipher
                        .decrypt_in_place(&mut self.reading.prefix[index..index + 1])
                        .map_err(TransportError::Crypto)?;
                }
                let byte = self.reading.prefix[index];
                self.reading.length |= usize::from(byte & 127) << (index * 7);
                self.reading.prefix_length += 1;
                if byte & 128 != 0 {
                    if self.reading.prefix_length == 3 {
                        return Err(TransportError::FrameLength);
                    }
                    continue;
                }
                if self.reading.length == 0
                    || self.reading.length > self.limits.max_frame_body_bytes
                {
                    return Err(TransportError::FrameLength);
                }
                self.reading.header_complete = true;
            }
            if self.reading.pending.is_none() {
                let threshold = self.compression_threshold()?.unwrap_or(-1);
                let size = self.reading.prefix_length + self.reading.length;
                let mut pending = self
                    .cpu
                    .try_reserve_packet(
                        PacketOperation::Decode { threshold },
                        size,
                        self.codec_limits(),
                    )
                    .map_err(TransportError::Admission)?;
                pending.input_mut()[..self.reading.prefix_length]
                    .copy_from_slice(&self.reading.prefix[..self.reading.prefix_length]);
                self.reading.read = self.reading.prefix_length;
                self.reading.pending = Some(pending);
            }
            if self.reading.read < self.reading.prefix_length + self.reading.length {
                let read = timeout_at(
                    until,
                    self.stream.as_mut().unwrap().read(
                        &mut self.reading.pending.as_mut().unwrap().input_mut()
                            [self.reading.read..],
                    ),
                )
                .await;
                let count = match read {
                    Ok(result) => result.map_err(TransportError::Io)?,
                    Err(_) if until == idle_deadline => return Err(TransportError::Timeout),
                    Err(_) => return Ok(None),
                };
                if count == 0 {
                    return Err(TransportError::Io(io::Error::from(
                        io::ErrorKind::UnexpectedEof,
                    )));
                }
                self.last_read = Instant::now();
                self.reading.read += count;
                continue;
            }
            let pending = self.reading.pending.take().unwrap();
            self.reading.task = Some(
                if let Some(cipher) = self.decryptor.take() {
                    pending.submit_with_decrypt(cipher, self.reading.prefix_length)
                } else {
                    pending.submit()
                }
                .map_err(TransportError::Admission)?,
            );
        }
    }

    /// Serializes compression, encryption and socket publication in owner order.
    /// The output lease remains live until the complete write (or error/drop).
    pub async fn write_packet(&mut self, packet: &[u8]) -> Result<(), TransportError> {
        let mut stream = self.stream.take().ok_or(TransportError::Closed)?;
        let encryptor = self.encryptor.take();
        let threshold = self.compression_threshold()?.unwrap_or(-1);
        let mut pending = self
            .cpu
            .try_reserve_packet(
                PacketOperation::Encode { threshold },
                packet.len(),
                self.codec_limits(),
            )
            .map_err(TransportError::Admission)?;
        pending.input_mut().copy_from_slice(packet);
        let task = if let Some(cipher) = encryptor {
            pending.submit_with_encrypt(cipher)
        } else {
            pending.submit()
        }
        .map_err(TransportError::Admission)?;
        let mut result = task.wait().await.map_err(TransportError::Worker)?;
        timeout(self.limits.write_timeout, stream.write_all(result.bytes()))
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(TransportError::Io)?;
        self.encryptor = result.take_encrypt();
        self.stream = Some(stream);
        Ok(())
    }

    /// Set Compression is itself framed using the old mode and encrypted using
    /// the current stream. No await exists between successful write and state
    /// publication. Interrupted writes leave stream=None, making reuse fail.
    pub async fn set_compression(&mut self, threshold: i32) -> Result<(), TransportError> {
        if !self.reading.is_idle() {
            return Err(TransportError::FrameLength);
        }
        let mut packet = [0; 6];
        packet[0] = 3;
        let length = crate::wire::write_varint(threshold, &mut packet[1..])
            .map_err(|_| TransportError::FrameLength)?;
        self.write_packet(&packet[..length + 1]).await?;
        self.compression = CompressionState::new(threshold);
        Ok(())
    }
}

#[derive(Default)]
struct ReadProgress {
    prefix: [u8; 3],
    prefix_length: usize,
    length: usize,
    header_complete: bool,
    read: usize,
    pending: Option<PendingPacket>,
    task: Option<PacketTask>,
}

impl ReadProgress {
    fn is_idle(&self) -> bool {
        self.prefix_length == 0 && self.task.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{CpuPoolConfig, SECTION_JOB_BUFFER_BYTES, SectionKey, TestGate},
        world::section::{Registry, SectionCounts},
    };
    use std::sync::{Condvar, Mutex, mpsc};

    struct Release(Arc<TestGate>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    #[test]
    fn queued_decode_obeys_read_idle_and_retains_worker_admission_until_release() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let cpu = Arc::new(
                    CpuPool::new(CpuPoolConfig {
                        workers: 1,
                        max_jobs: 2,
                        buffer_bytes: 2 * SECTION_JOB_BUFFER_BYTES + 65536,
                    })
                    .unwrap(),
                );
                let (started, receiver) = mpsc::sync_channel(1);
                let gate = Arc::new(TestGate {
                    started,
                    released: Mutex::new(false),
                    changed: Condvar::new(),
                });
                let _release = Release(Arc::clone(&gate));
                let pending = cpu
                    .try_reserve_section(
                        SectionKey {
                            world_epoch: 1,
                            chunk_x: 0,
                            chunk_z: 0,
                            section_y: 0,
                            revision: 1,
                        },
                        Registry::new(16).unwrap(),
                        Registry::new(4).unwrap(),
                        SectionCounts {
                            non_empty_blocks: 0,
                            fluid_blocks: 0,
                        },
                    )
                    .unwrap();
                let blocker = pending.submit_with_gate(Arc::clone(&gate)).unwrap();
                receiver.recv_timeout(Duration::from_secs(2)).unwrap();
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let (client, server) = tokio::join!(
                    TcpStream::connect(listener.local_addr().unwrap()),
                    listener.accept()
                );
                let mut client = client.unwrap();
                let mut transport = ConnectionTransport::new(
                    server.unwrap().0,
                    Arc::clone(&cpu),
                    TransportLimits {
                        max_frame_body_bytes: 1024,
                        max_uncompressed_bytes: 1024,
                        read_idle_timeout: Duration::from_millis(50),
                        write_timeout: Duration::from_secs(1),
                    },
                );
                client.write_all(&[1, 0]).await.unwrap();
                assert!(
                    transport
                        .read_packet_until(Instant::now() + Duration::from_millis(10))
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(cpu.stats().in_flight, 2);
                assert!(matches!(
                    transport.read_packet().await,
                    Err(TransportError::Timeout)
                ));
                assert!(!transport.is_open());
                assert_eq!(cpu.stats().in_flight, 2);
                assert!(cpu.stats().reserved_buffer_bytes > SECTION_JOB_BUFFER_BYTES);
                assert!(client.read_u8().await.is_err());
                gate.release();
                drop(blocker);
                timeout(Duration::from_secs(2), async {
                    while cpu.stats().in_flight != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(cpu.stats().reserved_buffer_bytes, 0);
            });
    }
    use tokio::net::TcpListener;
}
