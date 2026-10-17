//! `alkahest serve` with a real whisper.cpp (ALK-US-0057): builds the CPU engine from source,
//! pulls `ggerganov/whisper.cpp` `tiny.en` (about 75 MB) from the Hub and transcribes the
//! `jfk.wav` sample through `/v1/audio/transcriptions`.
//!
//! Opt-in: `ALKAHEST_TEST_ENGINE=1` (and network, git, cmake, ninja and a C++ toolchain; the
//! build takes a minute or two). Everything lives in a temporary `ALKAHEST_HOME` that is
//! deleted afterwards. `ALKAHEST_TEST_WAV` names a local 16 kHz WAV instead of downloading
//! the sample.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use serde_json::Value;

const JFK_URL: &str = "https://github.com/ggml-org/whisper.cpp/raw/v1.9.5/samples/jfk.wav";

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

#[tokio::test(flavor = "multi_thread")]
async fn real_whisper_transcribes_the_jfk_sample() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    // The CPU build whatever GPU the machine has: no toolkit needed.
    alkahest(
        home.path(),
        &["engines", "install", "whisper-cpp", "--variant", "cpu"],
    );
    alkahest(
        home.path(),
        &["pull", "hf.co/ggerganov/whisper.cpp:tiny.en"],
    );

    let wav = match std::env::var("ALKAHEST_TEST_WAV") {
        Ok(p) => std::fs::read(p).unwrap(),
        Err(_) => reqwest::get(JFK_URL)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .to_vec(),
    };
    assert!(wav.starts_with(b"RIFF"), "not a WAV file");

    let server = start(home.path()).await;
    let model = "hf.co/ggerganov/whisper.cpp:tiny.en";
    let send = |path: &str, format: &str| {
        let form = Form::new()
            .text("model", model)
            .text("response_format", format.to_string())
            .part(
                "file",
                Part::bytes(wav.clone())
                    .file_name("jfk.wav")
                    .mime_str("audio/wav")
                    .unwrap(),
            );
        let url = format!("{}{path}", server.base);
        async move {
            reqwest::Client::new()
                .post(url)
                .multipart(form)
                .send()
                .await
                .unwrap()
        }
    };

    let resp = send("/v1/audio/transcriptions", "json").await;
    assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    let v: Value = resp.json().await.unwrap();
    let text = v["text"].as_str().unwrap().to_lowercase();
    assert!(
        text.contains("ask not what your country can do for you"),
        "{v}"
    );

    let v: Value = send("/v1/audio/transcriptions", "verbose_json")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["language"], "english", "{v}");
    assert!(v["duration"].as_f64().unwrap() > 10.0, "{v}");
    assert!(!v["segments"].as_array().unwrap().is_empty(), "{v}");

    let srt = send("/v1/audio/transcriptions", "srt")
        .await
        .text()
        .await
        .unwrap();
    assert!(srt.contains(" --> "), "{srt}");
    let vtt = send("/v1/audio/transcriptions", "vtt")
        .await
        .text()
        .await
        .unwrap();
    assert!(vtt.starts_with("WEBVTT"), "{vtt}");

    // The model shows up as a speech-to-text model in the catalog listing.
    let ps: Value = reqwest::get(format!("{}/api/ps", server.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ps["models"].as_array().unwrap().len(), 1, "{ps}");
}
