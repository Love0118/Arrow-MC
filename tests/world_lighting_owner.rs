//! Canonical Anvil residents, admitted shared CPU lighting and packet publication.
#[path = "common/world_registry_fixture.rs"]
mod registry_fixture;

use arrow_mc::{
    nbt::{self, Compound, NamedTag, Tag},
    runtime::{
        CpuPool, CpuPoolConfig, LightingCompletion, PendingLighting, ResidentLightingBudget,
    },
    server::{
        chunk_packet,
        light_snapshot::{ChangedFilters, Error as SnapshotError, PacketLightSnapshot},
    },
    world::{
        lighting::{
            LightBlock, LightError, LightKind, SourceLimits,
            block::BlockLightLimits,
            owner::{LightingDomain, LightingOwnerError},
            sky::SkyLimits,
            storage::StorageLimits as LightStorageLimits,
            work::{LightingLimits, SkyWorkLimits},
        },
        loading::{ChunkLoadingOwner, LoadDemand, LoadingLimits, LoadingReadOutcome},
        preparation::ChunkAddress,
        storage::{
            ChunkStore, StorageLimits,
            chunk::{DATA_VERSION, DimensionHeight},
            registry::ChunkRegistrySnapshot,
        },
    },
};
use serde_json::json;
use std::{fs, sync::Arc, time::Duration};
use tokio::time::timeout;

fn address(x: i32) -> ChunkAddress {
    ChunkAddress { x, z: 0 }
}
fn block(x: i32) -> LightBlock {
    LightBlock { x, y: 8, z: 8 }
}
fn height() -> DimensionHeight {
    DimensionHeight::new(0, 16).unwrap()
}
fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

fn compound(entries: impl IntoIterator<Item = (&'static str, Tag)>) -> Tag {
    let mut value = Compound::new();
    for (key, tag) in entries {
        value.insert(key.into(), tag).unwrap();
    }
    Tag::Compound(value)
}

struct Fixture {
    _bundle: registry_fixture::Fixture,
    registry: Arc<ChunkRegistrySnapshot>,
    cpu: Arc<CpuPool>,
    resident: ResidentLightingBudget,
    owner: ChunkLoadingOwner,
    store: ChunkStore,
}

impl Fixture {
    fn new(sky: bool, states: &[&str]) -> Self {
        Self::with_slots(sky, states, 8)
    }
    fn with_slots(sky: bool, states: &[&str], max_jobs: usize) -> Self {
        let mut bundle = registry_fixture::Fixture::from_data(
            json!({
                "state_count":4,"state_flags":[1,0,0,0],"blocks":[
                    {"id":"minecraft:air","default_state":0,"properties":[],"states":[0]},
                    {"id":"minecraft:bedrock","default_state":1,"properties":[],"states":[1]},
                    {"id":"test:bright","default_state":2,"properties":[],"states":[2]},
                    {"id":"test:dim","default_state":3,"properties":[],"states":[3]}
                ]
            }),
            json!([{"id":"minecraft:plains","protocol_id":0}]),
        );
        let mut materials = [[0u8; 16]; 4];
        materials[1][1] = 15;
        materials[2][0] = 15;
        materials[3][0] = 4;
        bundle
            .edit_lighting(|bytes| *bytes = registry_fixture::lighting_bytes(&materials, 2, &[14]));
        let registry = Arc::new(bundle.load());
        let path = bundle.root.join("region");
        fs::create_dir(&path).unwrap();
        let mut region = vec![0; 8192];
        for (x, state) in states.iter().enumerate() {
            let section = compound([
                ("Y", Tag::Byte(0)),
                (
                    "block_states",
                    compound([("palette", Tag::List(vec![Tag::String((*state).into())]))]),
                ),
            ]);
            let root = NamedTag {
                name: "canonical lighting owner fixture".into(),
                tag: compound([
                    ("DataVersion", Tag::Int(DATA_VERSION)),
                    ("xPos", Tag::Int(x as i32)),
                    ("zPos", Tag::Int(0)),
                    ("Status", Tag::String("minecraft:full".into())),
                    ("sections", Tag::List(vec![section])),
                ]),
            };
            let mut bytes = Vec::new();
            nbt::write_named(&root, &mut bytes, nbt::Limits::default()).unwrap();
            let sector = region.len() / 4096;
            let count = (bytes.len() + 5).div_ceil(4096);
            region[x * 4..x * 4 + 4]
                .copy_from_slice(&(((sector as u32) << 8) | count as u32).to_be_bytes());
            region.extend_from_slice(&((bytes.len() + 1) as u32).to_be_bytes());
            region.push(3);
            region.extend(bytes);
            region.resize((sector + count) * 4096, 0);
        }
        fs::write(path.join("r.0.0.mca"), region).unwrap();
        let cpu = Arc::new(
            CpuPool::new(CpuPoolConfig {
                workers: 2,
                max_jobs,
                buffer_bytes: 256 * 1024 * 1024,
            })
            .unwrap(),
        );
        let owner = Self::owner(&registry, sky);
        let store = ChunkStore::new(
            path,
            Arc::clone(&cpu),
            Arc::clone(&registry),
            height(),
            StorageLimits::default(),
            2,
        )
        .unwrap();
        Self {
            _bundle: bundle,
            registry,
            cpu,
            resident: ResidentLightingBudget::new(256 * 1024 * 1024),
            owner,
            store,
        }
    }
    fn owner(registry: &Arc<ChunkRegistrySnapshot>, sky: bool) -> ChunkLoadingOwner {
        ChunkLoadingOwner::new(
            17,
            Arc::clone(registry),
            height(),
            sky,
            LoadingLimits {
                max_chunks: 4,
                metadata_bytes: 65536,
            },
            4 * 1024 * 1024,
        )
        .unwrap()
    }
    async fn load(&mut self, x: i32) {
        let LoadDemand::Read(request) = self.owner.request(address(x)).unwrap() else {
            panic!("expected new request")
        };
        let LoadingReadOutcome::Decoded(output) =
            timeout(Duration::from_secs(5), request.read(&self.store))
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected decoded Anvil chunk")
        };
        self.owner.publish(output).unwrap();
    }
    fn begin(
        &self,
        domain: &mut LightingDomain,
        addresses: &[ChunkAddress],
        sky: bool,
    ) -> PendingLighting {
        domain
            .begin(
                &self.owner,
                addresses,
                SourceLimits::default(),
                limits(sky),
                &self.cpu,
            )
            .unwrap()
    }
}

fn limits(sky: bool) -> LightingLimits {
    let storage = LightStorageLimits {
        max_sections: 128,
        max_columns: 32,
        max_notifications: 1024,
        metadata_bytes: 2 * 1024 * 1024,
        layer_bytes: 2 * 1024 * 1024,
    };
    LightingLimits {
        max_chunks: 4,
        metadata_bytes: 65536,
        block: BlockLightLimits {
            checks: 32,
            decreases: 65536,
            increases: 65536,
            queue_bytes: 8 * 1024 * 1024,
        },
        block_storage: storage,
        sky: sky.then_some(SkyWorkLimits {
            engine: SkyLimits {
                checks: 32,
                queue_entries: 65536,
                source_chunks: 4,
                planned_writes: 65536,
            },
            storage,
            engine_bytes: 16 * 1024 * 1024,
        }),
    }
}

async fn finish(mut pending: PendingLighting) -> LightingCompletion {
    for _ in 0..10000 {
        let result = timeout(Duration::from_secs(5), pending.submit(2048).unwrap().wait())
            .await
            .unwrap()
            .unwrap();
        let progress = result
            .progress()
            .unwrap_or_else(|error| panic!("lighting failed: {error}"));
        if progress.complete {
            return result;
        }
        pending = result
            .into_pending()
            .unwrap_or_else(|_| panic!("incomplete result must resume"));
    }
    panic!("finite canonical lighting domain did not converge")
}

#[test]
fn adopted_light_releases_the_only_cpu_slot_before_real_packet_delivery() {
    use arrow_mc::server::{
        chunk_sender::{
            ChunkDeliveryQueue, ChunkSender, DeliveryLimits, SendReadyChunk, SenderLimits,
        },
        transport::{ConnectionTransport, TransportLimits},
    };
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
    };
    run(async {
        let mut fixture = Fixture::with_slots(false, &["test:bright", "minecraft:air"], 1);
        fixture.load(0).await;
        fixture.load(1).await;
        let mut domain = LightingDomain::new();
        let completion = finish(fixture.begin(&mut domain, &[address(0), address(1)], false)).await;
        let cpu_reserved = completion.reserved_bytes();
        let resident_bytes = completion.resident_bytes().unwrap();
        assert_eq!(fixture.cpu.stats().in_flight, 1);
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, cpu_reserved);
        assert_eq!(completion.light_level(LightKind::Block, block(8)), Some(15));
        assert_eq!(
            completion.light_level(LightKind::Block, block(16)),
            Some(14)
        );
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        assert_eq!(fixture.cpu.stats().in_flight, 0);
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
        assert_eq!(fixture.resident.stats().results, 1);
        assert_eq!(fixture.resident.stats().used_bytes, resident_bytes);
        {
            let ready = domain.ready(&fixture.owner).unwrap();
            assert_eq!(ready.height(), height());
            assert!(ready.has_chunk(address(1)));
            assert_eq!(ready.light_level(LightKind::Block, block(16)), Some(14));
            assert_eq!(ready.light_level(LightKind::Sky, block(16)), None);
            assert!(matches!(
                PacketLightSnapshot::from_ready(&ready, address(2), ChangedFilters::default(), 0),
                Err(SnapshotError::MissingChunk)
            ));
            let bridge = PacketLightSnapshot::from_ready(
                &ready,
                address(1),
                ChangedFilters::default(),
                4096,
            )
            .unwrap();
            assert!(bridge.light_data().sky_updates.is_empty());
            assert!(!bridge.light_data().block_updates.is_empty());
            let mut sections = Vec::with_capacity(65536);
            fixture
                .owner
                .section(address(1), 0)
                .unwrap()
                .write_network(&mut sections)
                .unwrap();
            let encoded = chunk_packet::encode(
                &bridge.chunk_packet(&[], &sections, &[]),
                fixture.registry.block_entity_type_count(),
                chunk_packet::Limits::default(),
            )
            .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (peer, accepted) = tokio::join!(
                TcpStream::connect(listener.local_addr().unwrap()),
                listener.accept()
            );
            let mut peer = peer.unwrap();
            let mut transport = ConnectionTransport::new(
                accepted.unwrap().0,
                Arc::clone(&fixture.cpu),
                TransportLimits::default(),
            );
            let mut sender = ChunkSender::new(
                false,
                SenderLimits {
                    max_pending: 1,
                    control_bytes: 4096,
                },
            )
            .unwrap();
            let mut delivery = ChunkDeliveryQueue::new(DeliveryLimits {
                max_groups: 1,
                max_bytes: 65536,
            })
            .unwrap();
            sender.mark_pending(address(1)).unwrap();
            {
                // This explicitly synthetic transport harness checks bytes and
                // ordering, without claiming a world/Play readiness transition.
                let mut plan = sender.begin_tick(1, address(1)).unwrap();
                plan.try_admit(
                    &mut delivery,
                    &[Some(SendReadyChunk {
                        position: address(1),
                        packet_bytes: &encoded,
                    })],
                )
                .unwrap();
            }
            let mut scratch = chunk_packet::batch_start();
            while let Some(intent) = delivery.front_packet() {
                transport
                    .write_packet(chunk_packet::delivery_bytes(intent, &mut scratch).unwrap())
                    .await
                    .unwrap();
                delivery.packet_written().unwrap();
            }
            for expected in [&[0x0c][..], encoded.as_slice(), &[0x0b, 1][..]] {
                let actual = timeout(Duration::from_secs(2), async {
                    let mut length = 0usize;
                    for offset in 0..3 {
                        let byte = peer.read_u8().await.unwrap();
                        length |= usize::from(byte & 127) << (offset * 7);
                        if byte & 128 == 0 {
                            break;
                        }
                        assert!(offset < 2);
                    }
                    let mut bytes = vec![0; length];
                    peer.read_exact(&mut bytes).await.unwrap();
                    bytes
                })
                .await
                .unwrap();
                assert_eq!(actual, expected);
            }
            assert_eq!(fixture.cpu.stats().in_flight, 0);
            assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
            assert_eq!(fixture.resident.stats().used_bytes, resident_bytes);
        }
        domain.cancel();
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
        assert_eq!(fixture.resident.stats().results, 0);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        assert!(domain.ready(&fixture.owner).is_none());
    });
}

#[test]
fn block_and_sky_must_both_complete_before_a_coherent_capability_is_available() {
    run(async {
        let mut fixture = Fixture::new(true, &["test:dim"]);
        fixture.load(0).await;
        let mut domain = LightingDomain::new();
        let pending = fixture.begin(&mut domain, &[address(0)], true);
        let paused = pending.submit(0).unwrap().wait().await.unwrap();
        assert!(!paused.progress().unwrap().complete);
        let reserved = paused.reserved_bytes();
        let rejected = domain
            .accept(&fixture.owner, paused, &fixture.resident)
            .unwrap_err();
        assert_eq!(rejected.reason, LightingOwnerError::Incomplete);
        assert!(domain.ready(&fixture.owner).is_none());
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, reserved);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        let completion = finish(
            rejected
                .completion
                .into_pending()
                .unwrap_or_else(|_| panic!("paused completion")),
        )
        .await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        let ready = domain.ready(&fixture.owner).unwrap();
        assert_eq!(ready.light_level(LightKind::Block, block(8)), Some(4));
        assert_eq!(ready.light_level(LightKind::Sky, block(8)), Some(15));
        assert_eq!(ready.light_level(LightKind::Sky, block(32)), None);
        let bridge =
            PacketLightSnapshot::from_ready(&ready, address(0), ChangedFilters::default(), 4096)
                .unwrap();
        assert!(!bridge.light_data().sky_updates.is_empty());
        assert!(!bridge.light_data().block_updates.is_empty());
    });
}

#[test]
fn new_domain_attempt_rejects_old_completion_and_preserves_its_admission() {
    run(async {
        let mut fixture = Fixture::new(false, &["test:bright", "minecraft:air"]);
        fixture.load(0).await;
        fixture.load(1).await;
        let mut domain = LightingDomain::new();
        let old = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        let old_bytes = old.reserved_bytes();
        let new = fixture.begin(&mut domain, &[address(1)], false);
        let new_bytes = new.reserved_bytes();
        let rejection = domain
            .accept(&fixture.owner, old, &fixture.resident)
            .unwrap_err();
        assert_eq!(rejection.reason, LightingOwnerError::StaleSource);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        assert_eq!(
            fixture.cpu.stats().reserved_buffer_bytes,
            old_bytes + new_bytes
        );
        assert!(domain.ready(&fixture.owner).is_none());
        drop(rejection);
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, new_bytes);
        domain
            .accept(&fixture.owner, finish(new).await, &fixture.resident)
            .unwrap();
        let ready = domain.ready(&fixture.owner).unwrap();
        assert!(!ready.has_chunk(address(0)));
        assert_eq!(ready.light_level(LightKind::Block, block(16)), Some(0));
    });
}

#[test]
fn canonical_add_remove_and_reload_invalidate_later_ready_reads() {
    run(async {
        let mut fixture = Fixture::new(false, &["minecraft:air", "test:bright"]);
        fixture.load(0).await;
        let mut domain = LightingDomain::new();
        // A new canonical resident invalidates even a previously absent neighbor.
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        assert!(domain.ready(&fixture.owner).is_some());
        fixture.load(1).await;
        assert!(domain.ready(&fixture.owner).is_none());
        let completion = finish(fixture.begin(&mut domain, &[address(0), address(1)], false)).await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        assert!(domain.ready(&fixture.owner).is_some());
        assert!(fixture.owner.remove_demand(address(1)));
        assert!(domain.ready(&fixture.owner).is_none());
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        fixture
            .owner
            .reload(Arc::clone(&fixture.registry), height(), false)
            .unwrap();
        assert!(domain.ready(&fixture.owner).is_none());
        domain.cancel();
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
    });
}

#[test]
fn changed_canonical_source_and_wrong_owner_reject_completed_payloads() {
    run(async {
        let mut fixture = Fixture::new(false, &["test:bright"]);
        fixture.load(0).await;
        let mut domain = LightingDomain::new();
        let complete = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        let other = Fixture::owner(&fixture.registry, false);
        let rejected = domain
            .accept(&other, complete, &fixture.resident)
            .unwrap_err();
        assert_eq!(rejected.reason, LightingOwnerError::StaleSource);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        domain
            .accept(&fixture.owner, rejected.completion, &fixture.resident)
            .unwrap();
        assert!(domain.ready(&other).is_none());
        let complete = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        fixture.owner.remove_demand(address(0));
        fixture.load(0).await;
        let rejected = domain
            .accept(&fixture.owner, complete, &fixture.resident)
            .unwrap_err();
        assert_eq!(rejected.reason, LightingOwnerError::StaleSource);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        assert!(domain.ready(&fixture.owner).is_none());
    });
}

#[test]
fn failed_relight_requests_revoke_previous_ready_and_cancel_rejects_late_results() {
    run(async {
        let mut fixture = Fixture::new(false, &["minecraft:air"]);
        fixture.load(0).await;
        let mut domain = LightingDomain::new();
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        assert!(domain.ready(&fixture.owner).is_some());
        assert!(matches!(
            domain.begin(
                &fixture.owner,
                &[address(0)],
                SourceLimits::default(),
                limits(true),
                &fixture.cpu
            ),
            Err(LightingOwnerError::WrongSkyMode)
        ));
        assert!(domain.ready(&fixture.owner).is_none());
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
        assert!(matches!(
            domain.begin(
                &fixture.owner,
                &[address(2)],
                SourceLimits::default(),
                limits(false),
                &fixture.cpu
            ),
            Err(LightingOwnerError::Source(LightError::MissingChunk))
        ));
        assert!(matches!(
            domain.begin(
                &fixture.owner,
                &[],
                SourceLimits::default(),
                limits(false),
                &fixture.cpu
            ),
            Err(LightingOwnerError::Source(LightError::InvalidLimits))
        ));
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        domain.cancel();
        let rejected = domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap_err();
        assert_eq!(rejected.reason, LightingOwnerError::MissingRequest);
        assert!(fixture.cpu.stats().reserved_buffer_bytes > 0);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        drop(rejected);
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
    });
}

#[test]
fn cpu_admission_failure_revokes_previous_publication_without_leaking_its_lease() {
    run(async {
        let mut fixture = Fixture::new(false, &["test:dim"]);
        fixture.load(0).await;
        let mut domain = LightingDomain::new();
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        assert!(domain.ready(&fixture.owner).is_some());
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
        assert!(fixture.resident.stats().used_bytes > 0);
        let small = CpuPool::new(CpuPoolConfig {
            workers: 1,
            max_jobs: 1,
            buffer_bytes: arrow_mc::runtime::SECTION_JOB_BUFFER_BYTES,
        })
        .unwrap();
        assert!(matches!(
            domain.begin(
                &fixture.owner,
                &[address(0)],
                SourceLimits::default(),
                limits(false),
                &small
            ),
            Err(LightingOwnerError::Admission(
                arrow_mc::runtime::AdmissionError::ByteLimit
            ))
        ));
        assert!(domain.ready(&fixture.owner).is_none());
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        assert_eq!(small.stats().reserved_buffer_bytes, 0);
        // Even an unavailable source is not captured until its source metadata
        // and kernel reservation are admitted. The budget error takes priority.
        assert!(matches!(
            domain.begin(
                &fixture.owner,
                &[address(3)],
                SourceLimits::default(),
                limits(false),
                &small
            ),
            Err(LightingOwnerError::Admission(
                arrow_mc::runtime::AdmissionError::ByteLimit
            ))
        ));
        assert_eq!(small.stats().reserved_buffer_bytes, 0);
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        assert_eq!(
            domain
                .ready(&fixture.owner)
                .unwrap()
                .light_level(LightKind::Block, block(8)),
            Some(4)
        );
    });
}

#[test]
fn resident_admission_failure_returns_the_same_cpu_charged_completion_for_retry() {
    run(async {
        let mut fixture = Fixture::with_slots(false, &["test:bright"], 1);
        fixture.load(0).await;
        let mut domain = LightingDomain::new();
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        let cpu_bytes = completion.reserved_bytes();
        let resident_bytes = completion.resident_bytes().unwrap();
        assert!(resident_bytes > 0);
        let small = ResidentLightingBudget::new(resident_bytes - 1);
        let rejection = domain
            .accept(&fixture.owner, completion, &small)
            .unwrap_err();
        assert_eq!(
            rejection.reason,
            LightingOwnerError::Adoption(arrow_mc::runtime::LightingAdoptionReason::ByteLimit)
        );
        assert_eq!(rejection.completion.reserved_bytes(), cpu_bytes);
        assert_eq!(
            rejection.completion.resident_bytes().unwrap(),
            resident_bytes
        );
        assert_eq!(
            rejection.completion.light_level(LightKind::Block, block(8)),
            Some(15)
        );
        assert_eq!(fixture.cpu.stats().in_flight, 1);
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, cpu_bytes);
        assert_eq!(small.stats().results, 0);
        assert_eq!(small.stats().used_bytes, 0);
        assert_eq!(small.stats().peak_bytes, 0);
        assert!(domain.ready(&fixture.owner).is_none());
        let exact = ResidentLightingBudget::new(resident_bytes);
        domain
            .accept(&fixture.owner, rejection.completion, &exact)
            .unwrap();
        assert_eq!(fixture.cpu.stats().in_flight, 0);
        assert_eq!(fixture.cpu.stats().reserved_buffer_bytes, 0);
        assert_eq!(exact.stats().results, 1);
        assert_eq!(exact.stats().used_bytes, resident_bytes);
        assert_eq!(exact.stats().peak_bytes, resident_bytes);
        assert_eq!(
            domain
                .ready(&fixture.owner)
                .unwrap()
                .light_level(LightKind::Block, block(8)),
            Some(15)
        );
        domain.cancel();
        assert_eq!(exact.stats().results, 0);
        assert_eq!(exact.stats().used_bytes, 0);
    });
}

#[test]
fn resident_capacity_is_shared_and_old_domain_retains_data_until_its_reader_scope_ends() {
    run(async {
        let mut fixture = Fixture::with_slots(false, &["test:bright", "test:dim"], 1);
        fixture.load(0).await;
        fixture.load(1).await;
        let mut first = LightingDomain::new();
        let first_output = finish(fixture.begin(&mut first, &[address(0)], false)).await;
        let first_bytes = first_output.resident_bytes().unwrap();
        let budget = ResidentLightingBudget::new(first_bytes);
        first.accept(&fixture.owner, first_output, &budget).unwrap();
        let mut second = LightingDomain::new();
        let second_output = finish(fixture.begin(&mut second, &[address(1)], false)).await;
        let second_bytes = second_output.resident_bytes().unwrap();
        assert!(second_bytes <= first_bytes);
        let rejection = second
            .accept(&fixture.owner, second_output, &budget.clone())
            .unwrap_err();
        assert_eq!(
            rejection.reason,
            LightingOwnerError::Adoption(arrow_mc::runtime::LightingAdoptionReason::ByteLimit)
        );
        assert_eq!(budget.stats().results, 1);
        assert_eq!(budget.stats().used_bytes, first_bytes);
        assert_eq!(fixture.cpu.stats().in_flight, 1);
        {
            let ready = first.ready(&fixture.owner).unwrap();
            let bridge = PacketLightSnapshot::from_ready(
                &ready,
                address(0),
                ChangedFilters::default(),
                4096,
            )
            .unwrap();
            let encoded = chunk_packet::encode(
                &bridge.chunk_packet(&[], &[], &[]),
                fixture.registry.block_entity_type_count(),
                chunk_packet::Limits::default(),
            )
            .unwrap();
            assert!(!encoded.is_empty());
            assert_eq!(ready.light_level(LightKind::Block, block(8)), Some(15));
            assert_eq!(budget.stats().used_bytes, first_bytes);
        }
        // ReadyLighting is intentionally borrow-only. The old domain cannot
        // be cancelled while its packet reader is live; after that scope the
        // destination capacity becomes available for this same CPU completion.
        first.cancel();
        assert_eq!(budget.stats().used_bytes, 0);
        second
            .accept(&fixture.owner, rejection.completion, &budget)
            .unwrap();
        assert_eq!(fixture.cpu.stats().in_flight, 0);
        assert_eq!(budget.stats().results, 1);
        assert_eq!(budget.stats().used_bytes, second_bytes);
        assert_eq!(
            second
                .ready(&fixture.owner)
                .unwrap()
                .light_level(LightKind::Block, block(16)),
            Some(4)
        );
        drop(second);
        assert_eq!(budget.stats().used_bytes, 0);
    });
}

#[test]
fn stale_adopted_domain_keeps_canonical_resident_payload_until_cancel_after_unload() {
    run(async {
        let mut fixture = Fixture::with_slots(false, &["test:bright"], 1);
        fixture.load(0).await;
        let mut domain = LightingDomain::new();
        let completion = finish(fixture.begin(&mut domain, &[address(0)], false)).await;
        domain
            .accept(&fixture.owner, completion, &fixture.resident)
            .unwrap();
        let canonical_bytes = fixture.owner.stats().resident_bytes;
        let lighting_bytes = fixture.resident.stats().used_bytes;
        assert!(canonical_bytes > 0 && lighting_bytes > 0);
        {
            let ready = domain.ready(&fixture.owner).unwrap();
            let bridge = PacketLightSnapshot::from_ready(
                &ready,
                address(0),
                ChangedFilters::default(),
                4096,
            )
            .unwrap();
            assert!(!bridge.light_data().block_updates.is_empty());
            assert_eq!(fixture.owner.stats().resident_bytes, canonical_bytes);
            assert_eq!(fixture.resident.stats().used_bytes, lighting_bytes);
            assert_eq!(fixture.cpu.stats().in_flight, 0);
        }
        assert!(fixture.owner.remove_demand(address(0)));
        assert!(fixture.owner.resident(address(0)).is_none());
        assert!(domain.ready(&fixture.owner).is_none());
        // The invalidated result is no longer readable, but still owns its
        // immutable source until the lighting owner explicitly retires it.
        assert_eq!(fixture.owner.stats().resident_bytes, canonical_bytes);
        assert_eq!(fixture.resident.stats().used_bytes, lighting_bytes);
        assert_eq!(fixture.cpu.stats().in_flight, 0);
        domain.cancel();
        assert_eq!(fixture.owner.stats().resident_bytes, 0);
        assert_eq!(fixture.resident.stats().used_bytes, 0);
        assert_eq!(fixture.resident.stats().results, 0);
    });
}
