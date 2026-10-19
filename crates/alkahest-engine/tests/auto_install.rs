//! Install-on-first-use hook and bundled engine roots (ALK-T-0222).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alkahest_engine::{
    EngineError, EngineId, EngineInstaller, EngineKey, EngineRegistry, FsLocator, InstallFuture,
    InstallSource, LaunchRequest, LocateQuery, Os, Platform,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sysinfo::Accelerator;

fn platform() -> Platform {
    Platform {
        os: Os::Linux,
        aarch64: false,
        accelerator: Accelerator::Cpu,
    }
}

fn request() -> LaunchRequest {
    LaunchRequest::new("m", "/m/m.gguf", ModelFormat::Gguf, ModelKind::Chat)
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// Writes an executable placeholder where an installed engine would be.
fn fake_install(dir: &std::path::Path) {
    let root = dir.join("llama-cpp").join("b1-cpu");
    std::fs::create_dir_all(&root).unwrap();
    let bin = root.join(exe_name());
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[derive(Debug)]
struct FakeInstaller {
    dir: PathBuf,
    calls: AtomicUsize,
}

impl EngineInstaller for FakeInstaller {
    fn install(&self, engine: EngineKey) -> InstallFuture<'_> {
        Box::pin(async move {
            assert_eq!(engine, EngineKey::from(EngineId::LlamaCpp));
            self.calls.fetch_add(1, Ordering::SeqCst);
            fake_install(&self.dir);
            Ok(())
        })
    }
}

fn registry(dir: &std::path::Path) -> EngineRegistry {
    let loc = FsLocator::new(dir, BTreeMap::new()).with_search_path(None);
    EngineRegistry::with_builtin_engines(Arc::new(loc))
}

#[tokio::test]
async fn missing_engine_is_installed_once_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let installer = Arc::new(FakeInstaller {
        dir: tmp.path().to_path_buf(),
        calls: AtomicUsize::new(0),
    });
    let reg = registry(tmp.path()).with_auto_install(installer.clone());
    assert!(reg.auto_install_enabled());
    let plan = reg.plan_or_install(&request(), &platform()).await.unwrap();
    assert_eq!(plan.choice.engine, EngineId::LlamaCpp);
    assert_eq!(installer.calls.load(Ordering::SeqCst), 1);
    // Already installed now: no second install.
    reg.plan_or_install(&request(), &platform()).await.unwrap();
    assert_eq!(installer.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disabled_by_default_keeps_the_install_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = registry(tmp.path());
    assert!(!reg.auto_install_enabled());
    let err = reg
        .plan_or_install(&request(), &platform())
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::NotInstalled { .. }), "{err}");
    assert!(
        err.to_string()
            .contains("alkahest engines install llama-cpp")
    );
}

#[tokio::test]
async fn install_failure_is_reported() {
    #[derive(Debug)]
    struct Failing;
    impl EngineInstaller for Failing {
        fn install(&self, engine: EngineKey) -> InstallFuture<'_> {
            Box::pin(async move {
                Err(EngineError::AutoInstallFailed {
                    engine,
                    reason: "offline".into(),
                })
            })
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let reg = registry(tmp.path()).with_auto_install(Arc::new(Failing));
    let err = reg
        .plan_or_install(&request(), &platform())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("offline"), "{err}");
}

#[test]
fn bundled_roots_are_searched_after_the_engines_dir() {
    let engines = tempfile::tempdir().unwrap();
    let bundled = tempfile::tempdir().unwrap();
    fake_install(bundled.path());
    let loc = FsLocator::new(engines.path(), BTreeMap::new())
        .with_search_path(None)
        .with_extra_roots(vec![bundled.path().to_path_buf()]);
    let reg = EngineRegistry::with_builtin_engines(Arc::new(loc));
    let found = reg
        .locate(
            EngineId::LlamaCpp,
            &LocateQuery::for_accelerator(Accelerator::Cpu),
        )
        .unwrap();
    assert_eq!(found.source, InstallSource::Managed);
    assert!(found.binary.starts_with(bundled.path()));
}
