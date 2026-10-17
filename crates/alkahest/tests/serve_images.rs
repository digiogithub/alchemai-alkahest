//! `alkahest serve` with a real stable-diffusion.cpp (ALK-US-0059): installs the pinned release,
//! pulls SD-Turbo (`Green-Sky/SD-Turbo-GGUF`, 2.0 GB) from the Hub and generates a picture
//! through `/v1/images/generations`.
//!
//! Opt-in: `ALKAHEST_TEST_ENGINE=1` and network. By default everything lives in a temporary
//! `ALKAHEST_HOME` deleted afterwards. `ALKAHEST_TEST_HOME` names a directory to keep between runs
//! (the 2 GB model stays there; the caller deletes it), and `ALKAHEST_TEST_SD_VARIANT` picks the
//! engine variant (default `cpu`, the prebuilt archive; `cuda12.4` builds from source).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const MODEL: &str = "hf.co/Green-Sky/SD-Turbo-GGUF:sd_turbo-f16-q8_0.gguf";

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    /// SIGTERM, so the engine stops with the server.
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        for _ in 0..100 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn alkahest(home: &Path, args: &[&str]) {
    let status = Command::new(env!("CARGO_BIN_EXE_alkahest"))
        .args(args)
        .env("ALKAHEST_HOME", home)
        .status()
        .unwrap();
    assert!(status.success(), "alkahest {args:?}: {status}");
}

async fn start(home: &Path) -> Server {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = Server {
        child: Command::new(env!("CARGO_BIN_EXE_alkahest"))
            .args(["serve", "--host", &format!("127.0.0.1:{port}")])
            .env("ALKAHEST_HOME", home)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
        base: format!("http://127.0.0.1:{port}"),
    };
    for _ in 0..100 {
        if reqwest::get(format!("{}/api/version", server.base))
            .await
            .is_ok()
        {
            return server;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("alkahest serve did not start");
}

fn decode(b64: &str) -> Vec<u8> {
    // Minimal base64 decoder: the test has no base64 dependency.
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in b64.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn real_sd_turbo_generates_a_picture() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let home = std::env::var("ALKAHEST_TEST_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| temp.path().to_path_buf());
    let variant = std::env::var("ALKAHEST_TEST_SD_VARIANT").unwrap_or_else(|_| "cpu".into());
    alkahest(
        &home,
        &[
            "engines",
            "install",
            "stable-diffusion-cpp",
            "--variant",
            &variant,
        ],
    );
    alkahest(&home, &["pull", MODEL]);

    let server = start(&home).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1200))
        .build()
        .unwrap();
    let resp = client
        .post(format!("{}/v1/images/generations", server.base))
        .json(&json!({
            "model": MODEL, "prompt": "a red fox sitting in the snow, photo",
            "size": "512x512", "n": 1, "steps": 4, "cfg_scale": 1.0, "seed": 1,
            "sampler": "euler_a"
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v: Value = resp.json().await.unwrap();
    assert!(status.is_success(), "{status} {v}");
    let png = decode(v["data"][0]["b64_json"].as_str().unwrap());
    assert!(
        png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "not a PNG: {:?}",
        &png[..8.min(png.len())]
    );
    // 512x512 in the IHDR chunk.
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 512);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 512);
    assert!(
        png.len() > 20_000,
        "suspiciously small image: {} bytes",
        png.len()
    );

    if let Ok(path) = std::env::var("ALKAHEST_TEST_SAVE_PNG") {
        std::fs::write(path, &png).unwrap();
    }

    // The model is listed as an image model.
    let ls = Command::new(env!("CARGO_BIN_EXE_alkahest"))
        .args(["ls"])
        .env("ALKAHEST_HOME", &home)
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&ls.stdout);
    let row = listing
        .lines()
        .find(|l| l.contains("SD-Turbo-GGUF"))
        .unwrap_or_default();
    assert!(row.contains(" image "), "{listing}");
}
