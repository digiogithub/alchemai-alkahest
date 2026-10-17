//! The production path end to end: a model in a real (temporary) Alkahest store, resolved by
//! the catalog, planned by the engine registry and launched by the supervisor as a real
//! process (`alkahest-fake-engine` from `alkahest-engine`, standing in for llama-server).
//!
//! The fake engine binary is built by `cargo nextest run --workspace` / `cargo test
//! --workspace` (it is a bin target of `alkahest-engine`). When the test runs alone and the
//! binary is missing it says so and returns.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alkahest_core::KeepAlive;
use alkahest_core::config::EngineOverride;
use alkahest_engine::{ApiDialect, EngineId, EngineRegistry, FsLocator, Os, Platform};
use alkahest_sched::{CatalogBackend, Scheduler, SchedulerOptions};
use alkahest_store::{
    AlkahestStore, Catalog, Descriptor, Manifest, ModelOverlays, ModelRef, ModelSettings,
    media_type,
};
use alkahest_sysinfo::Accelerator;

/// `target/<profile>/alkahest-fake-engine`, next to the test binary's `deps/` directory.
fn fake_engine() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    let bin = dir.join(format!(
        "alkahest-fake-engine{}",
        std::env::consts::EXE_SUFFIX
    ));
    bin.exists().then_some(bin)
}

fn put_str(out: &mut Vec<u8>, v: &str) {
    out.extend((v.len() as u64).to_le_bytes());
    out.extend(v.as_bytes());
}

/// A tiny GGUF v3 header with the metadata the memory profile needs and one block tensor.
fn tiny_gguf() -> Vec<u8> {
    let u32s = [
        ("llama.block_count", 1u32),
        ("llama.context_length", 2048),
        ("llama.embedding_length", 64),
        ("llama.attention.head_count", 4),
        ("llama.attention.head_count_kv", 4),
    ];
    let mut o = b"GGUF".to_vec();
    o.extend(3u32.to_le_bytes());
    o.extend(1u64.to_le_bytes()); // tensors
    o.extend(((u32s.len() + 1) as u64).to_le_bytes());
    put_str(&mut o, "general.architecture");
    o.extend(8u32.to_le_bytes());
    put_str(&mut o, "llama");
    for (k, v) in u32s {
        put_str(&mut o, k);
        o.extend(4u32.to_le_bytes());
        o.extend(v.to_le_bytes());
    }
    put_str(&mut o, "blk.0.attn_q.weight");
    o.extend(2u32.to_le_bytes());
    o.extend(256u64.to_le_bytes());
    o.extend(64u64.to_le_bytes());
    o.extend(12u32.to_le_bytes()); // Q4_K
    o.extend(0u64.to_le_bytes());
    while !o.len().is_multiple_of(32) {
        o.push(0);
    }
    o.extend(vec![0u8; 144 * 64]);
    o
}

fn catalog(root: &std::path::Path) -> Catalog {
    let store = AlkahestStore::open(root.join("store")).unwrap();
    let config = store
        .put_json_blob(
            media_type::CONFIG,
            &serde_json::json!({"model_format":"gguf","model_family":"llama","file_type":"Q4_K_M"}),
        )
        .unwrap();
    let gguf = tiny_gguf();
    let layers = vec![Descriptor::new(
        media_type::MODEL,
        store.put_blob(&gguf).unwrap(),
        gguf.len() as u64,
    )];
    let name = "tiny:1b"
        .parse::<ModelRef>()
        .unwrap()
        .store_name()
        .unwrap()
        .unwrap();
    store
        .write_manifest(&name, &Manifest::new(config, layers))
        .unwrap();
    Catalog::new(store, None, Some(root.join("cache")))
        .with_overlays(ModelOverlays::new(root.join("model-config")))
}

#[tokio::test]
async fn idle_unload_stops_the_real_engine_process() {
    let Some(fake) = fake_engine() else {
        eprintln!("alkahest-fake-engine is not built; run the workspace tests to include this one");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "llama-cpp".to_string(),
        EngineOverride {
            path: Some(fake),
            extra_args: vec![],
        },
    );
    let locator = FsLocator::new(tmp.path().join("engines"), overrides).with_search_path(None);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(locator));
    let platform = Platform {
        os: Os::Linux,
        aarch64: false,
        accelerator: Accelerator::Cpu,
    };
    let backend = CatalogBackend::new(
        Arc::new(catalog(tmp.path())),
        Arc::new(registry),
        platform,
        ModelSettings::default(),
    )
    .with_log_dir(tmp.path().join("logs"));
    let opts = SchedulerOptions {
        keep_alive: KeepAlive::For(Duration::from_millis(300)),
        ..SchedulerOptions::default()
    };
    let s = Scheduler::new(Arc::new(backend), opts).unwrap();

    let lease = s
        .acquire("tiny:1b", &ModelSettings::default(), None)
        .await
        .unwrap();
    assert_eq!(lease.model(), "tiny:1b");
    assert_eq!(lease.engine(), EngineId::LlamaCpp);
    assert_eq!(lease.dialect(), ApiDialect::LlamaServer);
    assert!(lease.digest().starts_with("sha256:"));
    assert!(lease.pid().is_some());
    let port = lease.port();
    assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
    // The trained context (2048) caps the default context (4096).
    assert_eq!(lease.context_length(), 2048);
    assert!(tmp.path().join("logs").join("tiny_1b.log").exists());
    drop(lease);

    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.loaded().is_empty() {
        assert!(Instant::now() < deadline, "model was not unloaded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(
            Instant::now() < deadline,
            "engine still listening after unload"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
