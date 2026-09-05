//! Concrete section preparation and scaling probe; no world/gameplay simulation.
//!
//! `cargo run --release --example prepare_sections -- WORKERS SECTIONS IN_FLIGHT PALETTE`
//! WORKERS=0 uses the synchronous kernel as baseline. Synthetic registry IDs are
//! explicitly all non-air and have no fluid; this is not a real block registry.

use arrow_mc::runtime::{
    AdmissionError, CpuPool, CpuPoolConfig, SECTION_INPUT_BYTES, SECTION_JOB_BUFFER_BYTES,
    SectionKey, SectionTask, WORKER_STACK_BYTES,
};
use arrow_mc::world::section::{
    MAX_SECTION_NETWORK_BYTES, Registry, SectionCounts, prepare_section,
};
use std::collections::VecDeque;
use std::error::Error;
use std::time::{Duration, Instant};

fn argument(index: usize, default: usize) -> Result<usize, Box<dyn Error>> {
    std::env::args()
        .nth(index)
        .map_or(Ok(default), |text| Ok(text.parse()?))
}

fn fill(blocks: &mut [u32; 4096], biomes: &mut [u32; 64], sequence: usize, palette: u32) {
    for (index, id) in blocks.iter_mut().enumerate() {
        *id = ((index as u64 + sequence as u64 * 13) % u64::from(palette)) as u32;
    }
    for (index, id) in biomes.iter_mut().enumerate() {
        *id = ((index + sequence) % 8) as u32;
    }
}

fn consume(bytes: &[u8], checksum: &mut u64, byte_count: &mut usize) {
    *byte_count += bytes.len();
    // The preparation probe must not benchmark a serial full-payload hash.
    // Full byte equality is checked against the kernel in runtime_sections.rs;
    // this fingerprint and the byte count only confirm observable consumption.
    *checksum = checksum.wrapping_mul(0x100_0000_01b3) ^ bytes.len() as u64;
    for &byte in bytes.iter().take(8).chain(bytes.iter().rev().take(8)) {
        *checksum = checksum.wrapping_mul(0x100_0000_01b3) ^ u64::from(byte);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let workers = argument(1, 2)?;
    let sections = argument(2, 8192)?;
    let default_slots = workers
        .max(1)
        .checked_mul(4)
        .ok_or("worker count overflow")?;
    let slots = argument(3, default_slots)?;
    let palette = argument(4, 257)?;
    if sections == 0 || slots == 0 || !(1..=32768).contains(&palette) {
        return Err("sections/in-flight must be positive; palette must be 1..32768".into());
    }
    let registry = Registry::new(32768)?;
    let biomes = Registry::new(64)?;
    let counts = SectionCounts {
        non_empty_blocks: 4096,
        fluid_blocks: 0,
    };
    let mut checksum = 0xcbf2_9ce4_8422_2325u64;
    let mut output_bytes = 0usize;
    let mut latency = Vec::new();
    latency.try_reserve_exact(sections)?;
    let setup = Instant::now();
    // Model existing immutable source state. Only admitted worker-owned copies
    // are made during the timed path; source generation is not encoding work.
    let mut sources = Vec::new();
    sources.try_reserve_exact(8)?;
    for sequence in 0..8 {
        let mut source = ([0; 4096], [0; 64]);
        fill(&mut source.0, &mut source.1, sequence, palette as u32);
        sources.push(source);
    }
    let pool = if workers == 0 {
        None
    } else {
        Some(CpuPool::new(CpuPoolConfig {
            workers,
            max_jobs: slots,
            buffer_bytes: slots
                .checked_mul(SECTION_JOB_BUFFER_BYTES)
                .ok_or("buffer budget overflow")?,
        })?)
    };
    let setup_time = setup.elapsed();
    let start = Instant::now();
    let peak_reserved;
    let peak_running;
    if let Some(pool) = &pool {
        let mut pending: VecDeque<(Instant, SectionTask)> = VecDeque::new();
        pending.try_reserve_exact(slots)?;
        let mut submitted = 0usize;
        let mut consumed = 0usize;
        while consumed < sections {
            while submitted < sections {
                let key = SectionKey {
                    world_epoch: 1,
                    chunk_x: (submitted % 1024) as i32,
                    chunk_z: (submitted / 1024) as i32,
                    section_y: 0,
                    revision: submitted as u64,
                };
                let before = Instant::now();
                let mut job = match pool.try_reserve_section(key, registry, biomes, counts) {
                    Ok(job) => job,
                    Err(AdmissionError::JobLimit | AdmissionError::ByteLimit) => break,
                    Err(error) => return Err(error.into()),
                };
                let source = &sources[submitted % sources.len()];
                job.blocks_mut().copy_from_slice(&source.0);
                job.biomes_mut().copy_from_slice(&source.1);
                pending.push_back((before, job.submit()?));
                submitted += 1;
            }
            let (before, task) = pending.pop_front().ok_or("admission made no progress")?;
            let completed = task.wait().ok_or("section completion already taken")?;
            if completed.key().revision != consumed as u64 {
                return Err("owner submission order changed".into());
            }
            let bytes = completed.bytes().map_err(|error| error.to_string())?;
            consume(bytes, &mut checksum, &mut output_bytes);
            latency.push(before.elapsed());
            drop(completed);
            consumed += 1;
        }
        let stats = pool.stats();
        assert_eq!(stats.in_flight, 0);
        peak_reserved = stats.peak_reserved_buffer_bytes;
        peak_running = stats.peak_running;
    } else {
        let mut output = Vec::new();
        output.try_reserve_exact(MAX_SECTION_NETWORK_BYTES)?;
        for sequence in 0..sections {
            let before = Instant::now();
            let source = &sources[sequence % sources.len()];
            output.clear();
            prepare_section(&source.0, &source.1, registry, biomes, counts, &mut output)?;
            consume(&output, &mut checksum, &mut output_bytes);
            latency.push(before.elapsed());
        }
        peak_reserved = MAX_SECTION_NETWORK_BYTES;
        peak_running = 1;
    }
    let elapsed = start.elapsed();
    if let Some(pool) = pool {
        pool.shutdown().map_err(|_| "worker join failed")?;
    }
    latency.sort_unstable();
    let percentile = |percent: usize| -> Duration { latency[(latency.len() - 1) * percent / 100] };
    println!(
        "{{\"workers\":{workers},\"sections\":{sections},\"in_flight_limit\":{slots},\"palette\":{palette},\"setup_us\":{},\"elapsed_us\":{},\"sections_per_second\":{:.2},\"latency_p50_us\":{},\"latency_p95_us\":{},\"latency_p99_us\":{},\"output_bytes\":{output_bytes},\"sampled_checksum\":\"{checksum:016x}\",\"source_fixture_bytes\":{},\"peak_reserved_buffer_bytes\":{peak_reserved},\"worker_stack_reserved_bytes\":{},\"peak_running\":{peak_running}}}",
        setup_time.as_micros(),
        elapsed.as_micros(),
        sections as f64 / elapsed.as_secs_f64(),
        percentile(50).as_micros(),
        percentile(95).as_micros(),
        percentile(99).as_micros(),
        sources.len() * SECTION_INPUT_BYTES,
        workers * WORKER_STACK_BYTES,
    );
    Ok(())
}
