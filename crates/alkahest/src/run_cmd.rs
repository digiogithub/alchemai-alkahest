//! `alkahest run <model> [prompt]` (ALK-US-0039): chat with a model from the terminal, like
//! `ollama run`.
//!
//! * One-shot: a prompt on the command line, or input piped on stdin (piped text first, then
//!   the words, joined with spaces), goes to `/api/generate`; the answer streams to stdout
//!   and the command exits.
//! * Interactive: otherwise a line-edited session (rustyline, history in
//!   `<data_dir>/history`) keeps the conversation and sends it to `/api/chat`. Commands:
//!   `/bye`, `/clear`, `/set ...`, `/show ...`, `/load <model>`, `/?`; `"""` opens and closes
//!   a multi-line message; Ctrl+C stops a generation or clears the line, Ctrl+D leaves.
//!
//! The command is only a client: it never loads a model in-process. When the server is not
//! running it says how to start it and, on a terminal, offers to start `alkahest serve` in
//! the background. Thinking text goes to stderr so stdout carries only the answer.

use std::collections::VecDeque;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use alkahest_client::{
    ChatRequest, Client, ClientError, GenerateRequest, Message, ShowResponse, Stats,
};
use alkahest_core::config::Loaded;
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde_json::{Map, Value, json};

use crate::cli::RunArgs;
use crate::ps_cmd::{client, runtime};

/// Help text of the interactive session.
const HELP: &str = "\
Available commands:
  /set parameter <name> <value>  Set a model option (temperature, num_ctx, stop, ...)
  /set system <text>             Set the system message
  /set format json | noformat    Ask for JSON answers, or stop asking
  /set think [high|medium|low]   Turn thinking on (or set its level)
  /set nothink                   Turn thinking off
  /set verbose | quiet           Show or hide timing statistics
  /show info|parameters|system|template|license|modelfile
  /load <model>                  Switch to another model (clears the conversation)
  /clear                         Clear the conversation
  /bye                           Exit
  /?, /help                      This help

Use \"\"\" to begin and end a multi-line message.";

/// `alkahest run`.
pub fn run(args: &RunArgs, loaded: &Loaded, stats: bool) -> Result<ExitCode> {
    let c = client(loaded)?;
    let rt = runtime()?;
    let stdin_tty = std::io::stdin().is_terminal();
    if !ensure_server(&rt, &c, loaded, args.no_start || !stdin_tty)? {
        return Ok(ExitCode::FAILURE);
    }
    let show = match rt.block_on(c.show(&args.model)) {
        Ok(s) => s,
        Err(ClientError::Api { status: 404, .. }) => bail!(
            "model '{}' not found; pull it first with `alkahest pull {}`",
            args.model,
            args.model
        ),
        Err(e) => return Err(e.into()),
    };
    if show.capabilities.iter().any(|c| c == "embedding")
        && !show.capabilities.iter().any(|c| c == "completion")
    {
        bail!(
            "'{}' is an embedding model; `alkahest run` needs a model that generates text",
            args.model
        );
    }

    let mut session = Session::new(c, args, stats)?;
    session.show = Some(show);
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();

    let mut prompt = args.prompt.join(" ");
    if !stdin_tty {
        let mut piped = String::new();
        std::io::stdin()
            .read_to_string(&mut piped)
            .context("reading stdin")?;
        if !piped.trim().is_empty() {
            prompt = if prompt.is_empty() {
                piped
            } else {
                format!("{piped} {prompt}")
            };
        }
    }
    if !args.prompt.is_empty() || !stdin_tty {
        rt.block_on(session.generate(&prompt, &mut out, &mut err))?;
        return Ok(ExitCode::SUCCESS);
    }

    let history = loaded
        .config
        .resolve_paths(&loaded.paths)
        .data_dir
        .join("history");
    let mut lines = Editor::new(Some(history))?;
    rt.block_on(session.preload(&mut err))?;
    writeln!(err, "Type /? for help, /bye to exit.")?;
    repl(&rt, &mut session, &mut lines, &mut out, &mut err)?;
    Ok(ExitCode::SUCCESS)
}

/// Checks the server answers. When it does not: on a terminal, offers to start
/// `alkahest serve` in the background (unless `never_offer`); otherwise prints how to start
/// it. Returns whether a server is reachable now.
fn ensure_server(
    rt: &tokio::runtime::Runtime,
    c: &Client,
    loaded: &Loaded,
    never_offer: bool,
) -> Result<bool> {
    match rt.block_on(c.version()) {
        Ok(_) => return Ok(true),
        Err(ClientError::NotRunning { .. }) => {}
        Err(e) => return Err(e.into()),
    }
    let mut err = std::io::stderr();
    writeln!(err, "alkahest: no server is running at {}.", c.base_url())?;
    let interactive = !never_offer && std::io::stderr().is_terminal();
    if !interactive {
        writeln!(
            err,
            "Start it with `alkahest serve` (or install the service with `alkahest service install`)."
        )?;
        return Ok(false);
    }
    write!(err, "Start `alkahest serve` in the background now? [y/N] ")?;
    err.flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        writeln!(err, "Start it with `alkahest serve`.")?;
        return Ok(false);
    }
    let server = &loaded.config.server;
    let exe = std::env::current_exe().context("locating the alkahest binary")?;
    // Detached: the server outlives this session, like a service would.
    std::process::Command::new(exe)
        .args([
            "serve",
            "--host",
            &format!("{}:{}", server.host, server.port),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting `alkahest serve`")?;
    for _ in 0..150 {
        if rt.block_on(c.version()).is_ok() {
            writeln!(err, "Server started at {}.", c.base_url())?;
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    writeln!(
        err,
        "The server did not come up; see `alkahest serve` output."
    )?;
    Ok(false)
}

/// Parses `--think`: booleans stay booleans, anything else is a level string.
fn think_value(s: &str) -> Value {
    match s.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" => json!(true),
        "false" | "off" | "no" => json!(false),
        other => json!(other),
    }
}

/// Parses `--format`: `json`, or a JSON schema object.
fn format_value(s: &str) -> Result<Value> {
    if s.trim() == "json" {
        return Ok(json!("json"));
    }
    serde_json::from_str::<Value>(s)
        .ok()
        .filter(Value::is_object)
        .with_context(|| format!("--format must be `json` or a JSON schema, got {s:?}"))
}

/// Parses `--keepalive` and `/set parameter` values: numbers and booleans as such, the rest
/// as strings.
fn scalar(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = s.parse::<f64>() {
        return json!(f);
    }
    match s {
        "true" => json!(true),
        "false" => json!(false),
        _ => json!(s.trim_matches('"')),
    }
}

/// Conversation state and settings of one `run` session.
struct Session {
    client: Client,
    model: String,
    show: Option<ShowResponse>,
    messages: Vec<Message>,
    system: Option<String>,
    options: Map<String, Value>,
    format: Option<Value>,
    think: Option<Value>,
    keep_alive: Option<Value>,
    stats: bool,
    hide_thinking: bool,
}

/// What the REPL does after a command.
#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Exit,
}

impl Session {
    fn new(client: Client, args: &RunArgs, stats: bool) -> Result<Self> {
        Ok(Self {
            client,
            model: args.model.clone(),
            show: None,
            messages: Vec::new(),
            system: None,
            options: Map::new(),
            format: args.format.as_deref().map(format_value).transpose()?,
            think: args.think.as_deref().map(think_value),
            keep_alive: args.keepalive.as_deref().map(scalar),
            stats,
            hide_thinking: args.hidethinking,
        })
    }

    fn options(&self) -> Option<Value> {
        (!self.options.is_empty()).then(|| Value::Object(self.options.clone()))
    }

    /// Loads the model before the first prompt.
    async fn preload(&self, err: &mut impl Write) -> Result<()> {
        if let Err(e) = self.client.load(&self.model, self.keep_alive.clone()).await {
            writeln!(err, "error: {e}")?;
        }
        Ok(())
    }

    /// One-shot answer through `/api/generate`.
    async fn generate(
        &mut self,
        prompt: &str,
        out: &mut impl Write,
        err: &mut impl Write,
    ) -> Result<()> {
        let req = GenerateRequest {
            model: self.model.clone(),
            prompt: prompt.to_owned(),
            system: self.system.clone(),
            format: self.format.clone(),
            options: self.options(),
            keep_alive: self.keep_alive.clone(),
            think: self.think.clone(),
            ..GenerateRequest::default()
        };
        let mut stream = self.client.generate(&req).await?;
        let mut printer = Printer::new(self.hide_thinking);
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        let mut stats = None;
        loop {
            tokio::select! {
                biased;
                _ = &mut ctrl_c => break,
                item = stream.next() => match item {
                    None => break,
                    Some(chunk) => {
                        let chunk = chunk?;
                        printer.thinking(chunk.thinking.as_deref().unwrap_or(""), err)?;
                        printer.content(&chunk.response, out, err)?;
                        if chunk.done {
                            stats = Some(chunk.stats);
                        }
                    }
                }
            }
        }
        printer.finish(out)?;
        if self.stats
            && let Some(s) = stats
        {
            print_stats(&s, err)?;
        }
        Ok(())
    }

    /// One chat turn: sends the conversation plus `text`, streams the answer and keeps it.
    /// An interrupted answer is kept as far as it got (Ollama does the same).
    async fn chat(&mut self, text: &str, out: &mut impl Write, err: &mut impl Write) -> Result<()> {
        let mut messages = Vec::with_capacity(self.messages.len() + 2);
        if let Some(s) = &self.system {
            messages.push(Message::new("system", s.clone()));
        }
        messages.extend(self.messages.iter().cloned());
        messages.push(Message::new("user", text));
        let req = ChatRequest {
            model: self.model.clone(),
            messages,
            format: self.format.clone(),
            options: self.options(),
            keep_alive: self.keep_alive.clone(),
            think: self.think.clone(),
            ..ChatRequest::default()
        };
        let mut stream = match self.client.chat(&req).await {
            Ok(s) => s,
            Err(e) => {
                writeln!(err, "error: {e}")?;
                return Ok(());
            }
        };
        let mut printer = Printer::new(self.hide_thinking);
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        let mut answer = String::new();
        let mut stats = None;
        loop {
            tokio::select! {
                biased;
                _ = &mut ctrl_c => break,
                item = stream.next() => match item {
                    None => break,
                    Some(Err(e)) => {
                        printer.finish(out)?;
                        writeln!(err, "error: {e}")?;
                        return Ok(());
                    }
                    Some(Ok(chunk)) => {
                        let thinking = chunk.message.thinking.as_deref().unwrap_or("");
                        printer.thinking(thinking, err)?;
                        printer.content(&chunk.message.content, out, err)?;
                        answer.push_str(&chunk.message.content);
                        if chunk.done {
                            stats = Some(chunk.stats);
                        }
                    }
                }
            }
        }
        printer.finish(out)?;
        writeln!(out)?;
        self.messages.push(Message::new("user", text));
        self.messages.push(Message::new("assistant", answer));
        if self.stats
            && let Some(s) = stats
        {
            print_stats(&s, err)?;
        }
        Ok(())
    }

    /// Runs a `/command`.
    async fn command(&mut self, line: &str, out: &mut impl Write) -> Result<Flow> {
        let mut words = line.split_whitespace();
        let cmd = words.next().unwrap_or_default();
        let rest: Vec<&str> = words.collect();
        match cmd {
            "/bye" | "/exit" => return Ok(Flow::Exit),
            "/?" | "/help" => writeln!(out, "{HELP}")?,
            "/clear" => {
                self.messages.clear();
                writeln!(out, "Cleared session context")?;
            }
            "/set" => self.set(&rest, line, out)?,
            "/show" => self.show_cmd(rest.first().copied().unwrap_or(""), out)?,
            "/load" => match rest.first() {
                None => writeln!(out, "Usage: /load <model>")?,
                Some(model) => match self.client.show(model).await {
                    Ok(show) => {
                        self.model = (*model).to_owned();
                        self.show = Some(show);
                        self.messages.clear();
                        writeln!(out, "Loading model '{model}'")?;
                        if let Err(e) = self.client.load(model, self.keep_alive.clone()).await {
                            writeln!(out, "error: {e}")?;
                        }
                    }
                    Err(e) => writeln!(out, "error: {e}")?,
                },
            },
            other => writeln!(out, "Unknown command '{other}'. Type /? for help")?,
        }
        Ok(Flow::Continue)
    }

    fn set(&mut self, rest: &[&str], line: &str, out: &mut impl Write) -> Result<()> {
        match rest.first().copied() {
            Some("parameter") => {
                let (Some(name), Some(_)) = (rest.get(1), rest.get(2)) else {
                    writeln!(out, "Usage: /set parameter <name> <value>")?;
                    return Ok(());
                };
                let value = rest[2..].join(" ");
                if *name == "stop" {
                    let entry = self
                        .options
                        .entry("stop")
                        .or_insert_with(|| Value::Array(Vec::new()));
                    if let Value::Array(stops) = entry {
                        stops.extend(rest[2..].iter().map(|s| json!(s.trim_matches('"'))));
                    }
                } else {
                    self.options.insert((*name).to_owned(), scalar(&value));
                }
                writeln!(out, "Set parameter '{name}' to '{value}'")?;
            }
            Some("system") => {
                let text = line
                    .split_once("system")
                    .map(|(_, t)| t.trim().trim_matches('"').to_owned())
                    .unwrap_or_default();
                self.system = (!text.is_empty()).then_some(text);
                writeln!(out, "Set system message.")?;
            }
            Some("format") => match rest.get(1) {
                Some(&"json") => {
                    self.format = Some(json!("json"));
                    writeln!(out, "Set format to 'json' mode.")?;
                }
                _ => writeln!(out, "Usage: /set format json")?,
            },
            Some("noformat") => {
                self.format = None;
                writeln!(out, "Disabled format.")?;
            }
            Some("think") => {
                self.think = Some(rest.get(1).map_or(json!(true), |l| think_value(l)));
                writeln!(out, "Set 'think' mode.")?;
            }
            Some("nothink") => {
                self.think = Some(json!(false));
                writeln!(out, "Set 'nothink' mode.")?;
            }
            Some("verbose") => {
                self.stats = true;
                writeln!(out, "Set 'verbose' mode.")?;
            }
            Some("quiet") => {
                self.stats = false;
                writeln!(out, "Set 'quiet' mode.")?;
            }
            _ => writeln!(out, "Unknown /set option. Type /? for help")?,
        }
        Ok(())
    }

    fn show_cmd(&self, what: &str, out: &mut impl Write) -> Result<()> {
        let Some(show) = &self.show else {
            return Ok(());
        };
        let or_none = |s: &str| {
            if s.trim().is_empty() {
                "No value set.".to_owned()
            } else {
                s.trim_end().to_owned()
            }
        };
        match what {
            "info" => {
                writeln!(out, "  Model")?;
                let arch = show
                    .model_info
                    .get("general.architecture")
                    .and_then(Value::as_str)
                    .unwrap_or(&show.details.family);
                writeln!(out, "    architecture        {arch}")?;
                writeln!(
                    out,
                    "    parameters          {}",
                    show.details.parameter_size
                )?;
                if let Some(ctx) = show
                    .model_info
                    .iter()
                    .find(|(k, _)| k.ends_with(".context_length"))
                    .and_then(|(_, v)| v.as_u64())
                {
                    writeln!(out, "    context length      {ctx}")?;
                }
                writeln!(
                    out,
                    "    quantization        {}",
                    show.details.quantization_level
                )?;
                writeln!(out, "\n  Capabilities")?;
                for c in &show.capabilities {
                    writeln!(out, "    {c}")?;
                }
            }
            "parameters" => {
                writeln!(
                    out,
                    "Model defined parameters:\n{}",
                    or_none(&show.parameters)
                )?;
                if !self.options.is_empty() {
                    writeln!(out, "\nUser defined parameters:")?;
                    for (k, v) in &self.options {
                        writeln!(out, "{k:<20} {v}")?;
                    }
                }
            }
            "system" => {
                let s = self.system.as_deref().unwrap_or(&show.system);
                writeln!(out, "{}", or_none(s))?;
            }
            "template" => writeln!(out, "{}", or_none(&show.template))?,
            "license" => writeln!(out, "{}", or_none(&show.license))?,
            "modelfile" => writeln!(out, "{}", or_none(&show.modelfile))?,
            _ => writeln!(
                out,
                "Usage: /show info|parameters|system|template|license|modelfile"
            )?,
        }
        Ok(())
    }
}

/// Streams thinking (stderr, between `Thinking...` and `...done thinking.`) and content
/// (stdout).
struct Printer {
    hide_thinking: bool,
    in_thinking: bool,
    wrote_content: bool,
}

impl Printer {
    fn new(hide_thinking: bool) -> Self {
        Self {
            hide_thinking,
            in_thinking: false,
            wrote_content: false,
        }
    }

    fn thinking(&mut self, piece: &str, err: &mut impl Write) -> Result<()> {
        if piece.is_empty() || self.hide_thinking {
            return Ok(());
        }
        if !self.in_thinking {
            self.in_thinking = true;
            writeln!(err, "Thinking...")?;
        }
        write!(err, "{piece}")?;
        err.flush()?;
        Ok(())
    }

    fn content(&mut self, piece: &str, out: &mut impl Write, err: &mut impl Write) -> Result<()> {
        if piece.is_empty() {
            return Ok(());
        }
        if self.in_thinking {
            self.in_thinking = false;
            writeln!(err, "\n...done thinking.\n")?;
        }
        self.wrote_content = true;
        write!(out, "{piece}")?;
        out.flush()?;
        Ok(())
    }

    fn finish(&mut self, out: &mut impl Write) -> Result<()> {
        if self.wrote_content {
            writeln!(out)?;
        }
        out.flush()?;
        Ok(())
    }
}

/// Ollama's `--verbose` block.
fn print_stats(s: &Stats, err: &mut impl Write) -> Result<()> {
    let d = |ns: u64| format!("{:?}", Duration::from_nanos(ns));
    let rate = |n: u64, ns: u64| {
        if ns == 0 {
            "-".to_owned()
        } else {
            format!("{:.2} tokens/s", n as f64 / (ns as f64 / 1e9))
        }
    };
    writeln!(err, "total duration:       {}", d(s.total_duration))?;
    writeln!(err, "load duration:        {}", d(s.load_duration))?;
    writeln!(
        err,
        "prompt eval count:    {} token(s)",
        s.prompt_eval_count
    )?;
    writeln!(err, "prompt eval duration: {}", d(s.prompt_eval_duration))?;
    writeln!(
        err,
        "prompt eval rate:     {}",
        rate(s.prompt_eval_count, s.prompt_eval_duration)
    )?;
    writeln!(err, "eval count:           {} token(s)", s.eval_count)?;
    writeln!(err, "eval duration:        {}", d(s.eval_duration))?;
    writeln!(
        err,
        "eval rate:            {}",
        rate(s.eval_count, s.eval_duration)
    )?;
    Ok(())
}

/// One read from the line source.
enum Input {
    Line(String),
    Interrupted,
    Eof,
}

/// Where the REPL reads lines: the terminal editor, or a script in tests.
trait LineSource {
    fn read(&mut self, prompt: &str) -> Result<Input>;
    fn remember(&mut self, _line: &str) {}
}

/// rustyline with a history file.
struct Editor {
    rl: rustyline::DefaultEditor,
    history: Option<PathBuf>,
}

impl Editor {
    fn new(history: Option<PathBuf>) -> Result<Self> {
        let mut rl = rustyline::DefaultEditor::new().context("opening the line editor")?;
        if let Some(h) = &history {
            // A missing history file is the normal first run.
            let _ = rl.load_history(h);
        }
        Ok(Self { rl, history })
    }
}

impl LineSource for Editor {
    fn read(&mut self, prompt: &str) -> Result<Input> {
        use rustyline::error::ReadlineError;
        match self.rl.readline(prompt) {
            Ok(l) => Ok(Input::Line(l)),
            Err(ReadlineError::Interrupted) => Ok(Input::Interrupted),
            Err(ReadlineError::Eof) => Ok(Input::Eof),
            Err(e) => Err(e.into()),
        }
    }

    fn remember(&mut self, line: &str) {
        let _ = self.rl.add_history_entry(line);
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        if let Some(h) = &self.history {
            if let Some(dir) = h.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // History is a convenience; failing to save it must not fail the session.
            let _ = self.rl.save_history(h);
        }
    }
}

/// Scripted lines (tests and future non-terminal front ends).
#[cfg_attr(not(test), allow(dead_code))]
struct Scripted(VecDeque<String>);

impl LineSource for Scripted {
    fn read(&mut self, _prompt: &str) -> Result<Input> {
        Ok(self.0.pop_front().map_or(Input::Eof, Input::Line))
    }
}

/// Reads one message: a line, or a `"""` block spanning several lines. `None` at end of
/// input; `Some("")` when the line was interrupted.
fn read_message(lines: &mut dyn LineSource, err: &mut impl Write) -> Result<Option<String>> {
    let first = match lines.read(">>> ")? {
        Input::Line(l) => l,
        Input::Interrupted => {
            writeln!(err, "Use Ctrl + d or /bye to exit.")?;
            return Ok(Some(String::new()));
        }
        Input::Eof => return Ok(None),
    };
    let Some(after) = first.trim_start().strip_prefix("\"\"\"") else {
        return Ok(Some(first));
    };
    if let Some(body) = after.strip_suffix("\"\"\"") {
        return Ok(Some(body.to_owned()));
    }
    let mut text = after.to_owned();
    loop {
        match lines.read("... ")? {
            Input::Line(l) => {
                if let Some(body) = l.trim_end().strip_suffix("\"\"\"") {
                    text.push('\n');
                    text.push_str(body);
                    return Ok(Some(text.trim_start_matches('\n').to_owned()));
                }
                text.push('\n');
                text.push_str(&l);
            }
            Input::Interrupted => return Ok(Some(String::new())),
            Input::Eof => return Ok(Some(text)),
        }
    }
}

/// The interactive loop.
fn repl(
    rt: &tokio::runtime::Runtime,
    session: &mut Session,
    lines: &mut dyn LineSource,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    while let Some(text) = read_message(lines, err)? {
        if text.trim().is_empty() {
            continue;
        }
        lines.remember(&text);
        if text.trim_start().starts_with('/') {
            if rt.block_on(session.command(text.trim(), out))? == Flow::Exit {
                break;
            }
            continue;
        }
        rt.block_on(session.chat(&text, out, err))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn args(model: &str) -> RunArgs {
        RunArgs {
            model: model.into(),
            prompt: vec![],
            format: None,
            think: None,
            hidethinking: false,
            keepalive: None,
            no_start: true,
        }
    }

    fn scripted(lines: &[&str]) -> Scripted {
        Scripted(lines.iter().map(|s| (*s).to_owned()).collect())
    }

    const CHAT_STREAM: &str = concat!(
        r#"{"model":"m","message":{"role":"assistant","content":"","thinking":"hmm"},"done":false}"#,
        "\n",
        r#"{"model":"m","message":{"role":"assistant","content":"Hell"},"done":false}"#,
        "\n",
        r#"{"model":"m","message":{"role":"assistant","content":"o"},"done":false}"#,
        "\n",
        r#"{"model":"m","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","eval_count":2,"eval_duration":1000000000}"#,
        "\n"
    );

    #[test]
    fn multiline_blocks_and_interrupts() {
        let mut err = Vec::new();
        let mut l = scripted(&[
            "\"\"\"first",
            "second",
            "third\"\"\"",
            "\"\"\"one\"\"\"",
            "plain",
        ]);
        assert_eq!(
            read_message(&mut l, &mut err).unwrap().as_deref(),
            Some("first\nsecond\nthird")
        );
        assert_eq!(
            read_message(&mut l, &mut err).unwrap().as_deref(),
            Some("one")
        );
        assert_eq!(
            read_message(&mut l, &mut err).unwrap().as_deref(),
            Some("plain")
        );
        assert_eq!(read_message(&mut l, &mut err).unwrap(), None);
    }

    #[test]
    fn value_parsing() {
        assert_eq!(scalar("0.5"), json!(0.5));
        assert_eq!(scalar("4096"), json!(4096));
        assert_eq!(scalar("5m"), json!("5m"));
        assert_eq!(scalar("-1"), json!(-1));
        assert_eq!(think_value("high"), json!("high"));
        assert_eq!(think_value("false"), json!(false));
        assert_eq!(format_value("json").unwrap(), json!("json"));
        assert!(format_value(r#"{"type":"object"}"#).unwrap().is_object());
        assert!(format_value("yaml").is_err());
    }

    #[test]
    fn printer_separates_thinking_from_the_answer() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut p = Printer::new(false);
        p.thinking("let me see", &mut err).unwrap();
        p.content("42", &mut out, &mut err).unwrap();
        p.finish(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "42\n");
        let err = String::from_utf8(err).unwrap();
        assert!(err.starts_with("Thinking...\nlet me see"), "{err}");
        assert!(err.contains("...done thinking."), "{err}");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut p = Printer::new(true);
        p.thinking("secret", &mut err).unwrap();
        p.content("x", &mut out, &mut err).unwrap();
        assert!(err.is_empty());
    }

    #[test]
    fn repl_keeps_history_and_applies_commands() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        rt.block_on(
            Mock::given(method("POST"))
                .and(path("/api/chat"))
                .respond_with(ResponseTemplate::new(200).set_body_string(CHAT_STREAM))
                .mount(&server),
        );
        let client = Client::new(&server.uri()).unwrap();
        let mut session = Session::new(client, &args("m"), true).unwrap();
        let mut lines = scripted(&[
            "/set parameter temperature 0.5",
            "/set parameter stop <a> <b>",
            "/set system Be brief.",
            "/set think",
            "hello",
            "\"\"\"two",
            "lines\"\"\"",
            "/nope",
            "/clear",
            "again",
            "/bye",
            "never sent",
        ]);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        repl(&rt, &mut session, &mut lines, &mut out, &mut err).unwrap();
        let out = String::from_utf8(out).unwrap();
        let err = String::from_utf8(err).unwrap();
        assert!(out.contains("Hello\n"), "{out}");
        assert!(out.contains("Unknown command '/nope'"), "{out}");
        assert!(err.contains("eval rate:            2.00 tokens/s"), "{err}");
        assert!(err.contains("Thinking..."), "{err}");

        let reqs: Vec<Request> = rt.block_on(server.received_requests()).unwrap();
        let bodies: Vec<Value> = reqs.iter().map(|r| r.body_json().unwrap()).collect();
        assert_eq!(bodies.len(), 3);
        let first = &bodies[0];
        assert_eq!(first["options"]["temperature"], json!(0.5));
        assert_eq!(first["options"]["stop"], json!(["<a>", "<b>"]));
        assert_eq!(first["think"], json!(true));
        assert_eq!(
            first["messages"][0],
            json!({"role": "system", "content": "Be brief."})
        );
        assert_eq!(first["messages"][1]["content"], "hello");
        // The second turn carries the first exchange.
        let second = &bodies[1];
        assert_eq!(second["messages"].as_array().unwrap().len(), 4);
        assert_eq!(
            second["messages"][2],
            json!({"role": "assistant", "content": "Hello"})
        );
        assert_eq!(second["messages"][3]["content"], "two\nlines");
        // After /clear only the system message and the new prompt remain.
        assert_eq!(bodies[2]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(bodies[2]["messages"][1]["content"], "again");
    }
}
