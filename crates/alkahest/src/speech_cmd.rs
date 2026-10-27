//! `alkahest run <speech model>` (ALK-US-0143): reads text aloud, like `ollama run phonon`
//! (Ollama 0.40.11 `cmd/speech.go`).
//!
//! * Text arguments, piped input, or both (piped first): spoken a few sentences at a time as the
//!   text arrives, so a chat model piped in is heard while it still writes. The next piece is
//!   synthesised while the current one plays.
//! * No text on a terminal: a `>>>` prompt; each line is spoken, Ctrl+C stops speaking, `/bye` or
//!   Ctrl+D exits.
//! * stdout a terminal: played with the system's player (`afplay` on macOS; `paplay`, `pw-play`,
//!   `aplay` or `ffplay` on Linux; PowerShell's `Media.SoundPlayer` on Windows). Redirected: one
//!   WAV is written instead (`alkahest run phonon "text" > speech.wav`).

use std::io::{IsTerminal, Read, Write};
use std::path::Path;

use alkahest_client::{Client, SpeechRequest};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use tokio::sync::mpsc;

/// The most characters `/v1/audio/speech` takes at once.
pub const MAX_SPEECH_INPUT: usize = 4096;
/// Text gathered before a sentence is sent on, so speech starts soon without many tiny requests.
const MIN_SPEECH_PIECE: usize = 100;
/// Size of a WAV header as the server streams it.
const WAV_HEADER: usize = 44;

/// Index just past the last `.`, `!` or `?` in `s` that is followed by a space or newline.
pub fn last_sentence_end(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    (1..b.len().saturating_sub(1))
        .rev()
        .find(|&i| matches!(b[i], b'.' | b'!' | b'?') && matches!(b[i + 1], b' ' | b'\n'))
        .map(|i| i + 1)
}

/// Where `text` may be cut so the part before it is spoken now: after the last paragraph break
/// or sentence end, once a paragraph or at least [`MIN_SPEECH_PIECE`] characters arrived.
/// `None` waits for more.
pub fn speech_cut(text: &str) -> Option<usize> {
    let para = text.rfind("\n\n").filter(|&p| p > 0);
    let cut = para.max(last_sentence_end(text))?;
    (para.is_some() || text[..cut].chars().count() >= MIN_SPEECH_PIECE).then_some(cut)
}

/// One piece per paragraph; a paragraph over `limit` characters is cut at the last sentence end
/// or whitespace that fits (or at `limit` characters when there is none).
pub fn speech_chunks(text: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    for para in text.replace("\r\n", "\n").split("\n\n") {
        let mut p = para.trim();
        while !p.is_empty() {
            let mut end = p.len();
            if p.chars().count() > limit {
                end = p.char_indices().nth(limit).map_or(p.len(), |(i, _)| i);
                if let Some(i) = last_sentence_end(&p[..end]) {
                    end = i;
                } else if let Some(i) = p[..end].rfind([' ', '\t', '\n']).filter(|&i| i > 0) {
                    end = i;
                }
            }
            chunks.push(p[..end].trim().to_string());
            p = p[end..].trim();
        }
    }
    chunks
}

/// Splits text that arrives in pieces into speakable chunks (see [`speech_cut`]).
#[derive(Debug, Default)]
pub struct Splitter {
    pending: String,
}

impl Splitter {
    /// Adds `text`; returns what can be spoken now.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.pending.push_str(text);
        match speech_cut(&self.pending) {
            Some(cut) => {
                let rest = self.pending.split_off(cut);
                let ready = std::mem::replace(&mut self.pending, rest);
                speech_chunks(&ready, MAX_SPEECH_INPUT)
            }
            None => Vec::new(),
        }
    }

    /// The rest, once the input ended.
    pub fn finish(self) -> Vec<String> {
        speech_chunks(&self.pending, MAX_SPEECH_INPUT)
    }
}

/// The command that plays the WAV at `path` on `os` (`std::env::consts::OS`), using the first
/// player `available` finds.
pub fn player_command(
    os: &str,
    path: &Path,
    available: &dyn Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let p = path.to_string_lossy().into_owned();
    let candidates: Vec<Vec<String>> = match os {
        "macos" => vec![vec!["afplay".into(), p]],
        "windows" => vec![vec![
            "powershell".into(),
            "-NoProfile".into(),
            "-Command".into(),
            format!(
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                p.replace('\'', "''")
            ),
        ]],
        _ => vec![
            vec!["paplay".into(), p.clone()],
            vec!["pw-play".into(), p.clone()],
            vec!["aplay".into(), "-q".into(), p.clone()],
            vec![
                "ffplay".into(),
                "-nodisp".into(),
                "-autoexit".into(),
                "-loglevel".into(),
                "error".into(),
                p,
            ],
        ],
    };
    candidates.into_iter().find(|c| available(&c[0]))
}

/// Whether `name` is an executable on `PATH`.
fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let f = dir.join(name);
        f.is_file() || (cfg!(windows) && f.with_extension("exe").is_file())
    })
}

/// Writes the real sizes into a WAV the server streamed with unknown length (players need them).
pub fn set_wav_sizes(wav: &mut [u8]) {
    if wav.len() < WAV_HEADER {
        return;
    }
    let riff = u32::try_from(wav.len() - 8).unwrap_or(u32::MAX);
    let data = u32::try_from(wav.len() - WAV_HEADER).unwrap_or(u32::MAX);
    wav[4..8].copy_from_slice(&riff.to_le_bytes());
    wav[40..44].copy_from_slice(&data.to_le_bytes());
}

fn save_hint(model: &str) -> String {
    format!("To save the audio instead, run: alkahest run {model} \"text\" > speech.wav")
}

/// How the user runs a speech model.
pub struct Speech<'a> {
    /// Server client.
    pub client: &'a Client,
    /// Model name.
    pub model: &'a str,
    /// Voice; `None` is the model's default.
    pub voice: Option<&'a str>,
}

impl Speech<'_> {
    /// Entry point: `text` from the arguments, stdin piped or not.
    pub fn run(&self, rt: &tokio::runtime::Runtime, text: &str, stdin_tty: bool) -> Result<()> {
        if !stdin_tty {
            // Piped input as it arrives, then the text arguments.
            let (tx, rx) = mpsc::channel::<String>(16);
            let tail = text.to_string();
            std::thread::Builder::new()
                .name("alkahest-speech-stdin".into())
                .spawn(move || {
                    let mut buf = [0u8; 4096];
                    let mut stdin = std::io::stdin();
                    let mut carry = Vec::new();
                    loop {
                        match stdin.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                carry.extend_from_slice(&buf[..n]);
                                // Hand on only complete UTF-8; keep a split character.
                                let valid = match std::str::from_utf8(&carry) {
                                    Ok(s) => s.len(),
                                    Err(e) => e.valid_up_to(),
                                };
                                let s = String::from_utf8_lossy(&carry[..valid]).into_owned();
                                carry.drain(..valid);
                                if tx.blocking_send(s).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    let _ = tx.blocking_send(format!(" {tail}"));
                })
                .context("reading stdin")?;
            return rt.block_on(self.speak(rx));
        }
        if !text.trim().is_empty() {
            return rt.block_on(self.speak(one(text)));
        }
        if !std::io::stdout().is_terminal() {
            bail!(
                "speech models need text. Usage: alkahest run {} \"your text here\"",
                self.model
            );
        }
        self.prompt(rt)
    }

    /// The `>>>` prompt.
    fn prompt(&self, rt: &tokio::runtime::Runtime) -> Result<()> {
        use rustyline::error::ReadlineError;
        if player_command(std::env::consts::OS, Path::new("x.wav"), &on_path).is_none() {
            bail!("no audio player found. {}", save_hint(self.model));
        }
        let mut rl = rustyline::DefaultEditor::new().context("opening the line editor")?;
        eprintln!("Type text to speak; /bye to exit.");
        loop {
            let line = match rl.readline(">>> ") {
                Ok(l) => l,
                Err(ReadlineError::Eof) => return Ok(()),
                Err(ReadlineError::Interrupted) => {
                    eprintln!("Use Ctrl + d or /bye to exit.");
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let text = line.trim();
            if text == "/bye" {
                return Ok(());
            }
            if text.is_empty() {
                continue;
            }
            let _ = rl.add_history_entry(text);
            // Ctrl+C stops speaking and returns to the prompt.
            let outcome = rt.block_on(async {
                tokio::select! {
                    r = self.speak(one(text)) => Some(r),
                    _ = tokio::signal::ctrl_c() => None,
                }
            });
            match outcome {
                Some(Err(e)) => eprintln!("Error: {e:#}"),
                None => eprintln!(),
                Some(Ok(())) => {}
            }
        }
    }

    /// Speaks the text arriving on `input`: plays each piece, or writes one WAV to stdout.
    async fn speak(&self, mut input: mpsc::Receiver<String>) -> Result<()> {
        let to_terminal = std::io::stdout().is_terminal();
        if to_terminal
            && player_command(std::env::consts::OS, Path::new("x.wav"), &on_path).is_none()
        {
            bail!("no audio player found. {}", save_hint(self.model));
        }
        // Synthesise the next piece while the current one plays.
        let (audio_tx, mut audio_rx) = mpsc::channel::<Result<Vec<u8>>>(1);
        let client = self.client.clone();
        let model = self.model.to_string();
        let voice = self.voice.map(str::to_string);
        let producer = async move {
            let mut splitter = Splitter::default();
            let mut ended = false;
            while !ended {
                let pieces = match input.recv().await {
                    Some(text) => splitter.push(&text),
                    None => {
                        ended = true;
                        std::mem::take(&mut splitter).finish()
                    }
                };
                for piece in pieces {
                    let wav = synthesize(&client, &model, voice.as_deref(), &piece).await;
                    let failed = wav.is_err();
                    if audio_tx.send(wav).await.is_err() || failed {
                        return;
                    }
                }
            }
        };
        let consumer = async {
            let mut out: Vec<u8> = Vec::new();
            let mut spoken = false;
            while let Some(wav) = audio_rx.recv().await {
                let wav = wav?;
                spoken = true;
                if to_terminal {
                    play(self.model, &wav).await?;
                } else if out.is_empty() {
                    out = wav;
                } else {
                    out.extend_from_slice(&wav[WAV_HEADER..]);
                }
            }
            if !spoken {
                bail!(
                    "speech models need text. Usage: alkahest run {} \"your text here\"",
                    self.model
                );
            }
            if !to_terminal {
                set_wav_sizes(&mut out);
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(&out)?;
                stdout.flush()?;
            }
            Ok(())
        };
        let ((), r) = tokio::join!(producer, consumer);
        r
    }
}

/// A channel that yields `text` once.
fn one(text: &str) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(1);
    let _ = tx.try_send(text.to_string());
    rx
}

/// One WAV for `text`, with its sizes set.
async fn synthesize(
    client: &Client,
    model: &str,
    voice: Option<&str>,
    text: &str,
) -> Result<Vec<u8>> {
    let mut stream = client
        .speech(&SpeechRequest {
            model: model.to_string(),
            input: text.to_string(),
            voice: voice.map(str::to_string),
            response_format: Some("wav".into()),
        })
        .await?;
    let mut wav = Vec::new();
    while let Some(chunk) = stream.next().await {
        wav.extend_from_slice(&chunk?);
    }
    if wav.len() <= WAV_HEADER {
        bail!("the speech response contained no audio");
    }
    set_wav_sizes(&mut wav);
    Ok(wav)
}

/// Plays `wav` with the system player; killed when the caller stops waiting (Ctrl+C).
async fn play(model: &str, wav: &[u8]) -> Result<()> {
    let mut file = tempfile::Builder::new()
        .prefix("alkahest-")
        .suffix(".wav")
        .tempfile()
        .context("creating a temporary WAV file")?;
    file.write_all(wav)?;
    file.flush()?;
    let cmd = player_command(std::env::consts::OS, file.path(), &on_path)
        .with_context(|| format!("no audio player found. {}", save_hint(model)))?;
    let status = tokio::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .with_context(|| format!("running {}", cmd[0]))?;
    if !status.success() {
        bail!("{} failed ({status}). {}", cmd[0], save_hint(model));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn sentence_ends_need_following_whitespace() {
        assert_eq!(last_sentence_end("Hi. There"), Some(3));
        assert_eq!(last_sentence_end("Pi is 3.14 ok"), None);
        assert_eq!(last_sentence_end("Done!\nNext? yes"), Some(11));
        assert_eq!(
            last_sentence_end("End."),
            None,
            "the end itself waits for more"
        );
    }

    #[test]
    fn short_text_waits_and_paragraphs_go_at_once() {
        assert_eq!(speech_cut("Hello. World"), None, "under 100 characters");
        let long = format!("{}. Then more", "a".repeat(120));
        assert_eq!(speech_cut(&long), Some(121));
        assert_eq!(speech_cut("Short.\n\nNext"), Some(6));
    }

    #[test]
    fn chunks_split_paragraphs_and_respect_the_limit() {
        assert_eq!(
            speech_chunks("One.\r\n\r\nTwo.\n\n\n", 4096),
            ["One.", "Two."]
        );
        let text = "Aaaa bbbb. Cccc dddd eeee";
        assert_eq!(speech_chunks(text, 12), ["Aaaa bbbb.", "Cccc dddd", "eeee"]);
        let words = "é".repeat(10);
        assert_eq!(speech_chunks(&words, 4), ["éééé", "éééé", "éé"]);
    }

    #[test]
    fn splitter_streams_sentences_and_flushes_the_rest() {
        let mut s = Splitter::default();
        assert!(s.push("Hello there").is_empty());
        let first = s.push(&format!(" {}. And", "x".repeat(100)));
        assert_eq!(first.len(), 1);
        assert!(first[0].starts_with("Hello there") && first[0].ends_with('.'));
        assert_eq!(s.finish(), ["And"]);
    }

    #[test]
    fn players_per_platform() {
        let p = Path::new("/tmp/a b.wav");
        let all = |_: &str| true;
        assert_eq!(player_command("macos", p, &all).unwrap()[0], "afplay");
        assert_eq!(
            player_command("linux", p, &all).unwrap(),
            ["paplay", "/tmp/a b.wav"]
        );
        let only_aplay = |n: &str| n == "aplay";
        assert_eq!(
            player_command("linux", p, &only_aplay).unwrap(),
            ["aplay", "-q", "/tmp/a b.wav"]
        );
        assert_eq!(player_command("linux", p, &|_| false), None);
        let win = player_command("windows", Path::new(r"C:\it's.wav"), &all).unwrap();
        assert!(win[3].contains("'C:\\it''s.wav'"), "{win:?}");
    }

    #[test]
    fn wav_sizes_are_filled_in() {
        let mut wav = vec![0u8; 50];
        wav[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        set_wav_sizes(&mut wav);
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 42);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
    }
}
