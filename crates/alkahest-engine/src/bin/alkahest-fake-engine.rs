//! Test support only: a stand-in engine for the supervisor tests. It is not part of any
//! release bundle.
//!
//! Serves `GET /health` on `--host`/`--port` (503 `{"status":"loading"}` until
//! `--ready-after-ms`, then 200 `{"status":"ok"}`), prints a few log lines on both pipes and
//! can crash on purpose:
//!
//! * `--crash-after-ms N` exits with `--exit-code` (default 3) after N ms,
//! * `--exit-immediately` exits with `--exit-code` before listening,
//! * `--spawn-sleeper` forks a long `sleep` child (unix) so tests can prove the whole process
//!   group is stopped,
//! * `--pid-file PATH` writes the sleeper's pid there.
//!
//! A bind failure prints llama-server's wording and exits with code 1. Unknown arguments are
//! ignored so real engine command lines can be replayed against it.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Args {
    host: String,
    port: u16,
    ready_after: Duration,
    crash_after: Option<Duration>,
    exit_immediately: bool,
    exit_code: u8,
    spawn_sleeper: bool,
    pid_file: Option<String>,
}

fn parse() -> Args {
    let mut a = Args {
        host: "127.0.0.1".into(),
        port: 0,
        ready_after: Duration::ZERO,
        crash_after: None,
        exit_immediately: false,
        exit_code: 3,
        spawn_sleeper: false,
        pid_file: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let ms =
            |v: Option<String>| Duration::from_millis(v.and_then(|s| s.parse().ok()).unwrap_or(0));
        match flag.as_str() {
            "--host" => a.host = it.next().unwrap_or(a.host),
            "--port" => a.port = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--ready-after-ms" => a.ready_after = ms(it.next()),
            "--crash-after-ms" => a.crash_after = Some(ms(it.next())),
            "--exit-immediately" => a.exit_immediately = true,
            "--exit-code" => a.exit_code = it.next().and_then(|s| s.parse().ok()).unwrap_or(3),
            "--spawn-sleeper" => a.spawn_sleeper = true,
            "--pid-file" => a.pid_file = it.next(),
            _ => {}
        }
    }
    a
}

fn main() -> ExitCode {
    let args = parse();
    println!("fake-engine: starting with {args:?}");
    eprintln!("fake-engine: loading model");
    if args.exit_immediately {
        eprintln!("fake-engine: error: failed to load model");
        return ExitCode::from(args.exit_code);
    }
    let listener = match TcpListener::bind((args.host.as_str(), args.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "main: error: couldn't bind HTTP server socket, hostname: {}, port: {}: {e}",
                args.host, args.port
            );
            return ExitCode::from(1);
        }
    };
    eprintln!("fake-engine: listening on {}:{}", args.host, args.port);

    if args.spawn_sleeper {
        spawn_sleeper(args.pid_file.as_deref());
    }

    let started = Instant::now();
    if let Some(after) = args.crash_after {
        let code = args.exit_code;
        std::thread::spawn(move || {
            std::thread::sleep(after);
            eprintln!("fake-engine: fatal: simulated crash");
            std::process::exit(i32::from(code));
        });
    }

    for stream in listener.incoming().flatten() {
        let ready = started.elapsed() >= args.ready_after;
        std::thread::spawn(move || serve(stream, ready));
    }
    ExitCode::SUCCESS
}

fn serve(mut stream: TcpStream, ready: bool) {
    let mut first = String::new();
    {
        let mut reader = BufReader::new(&stream);
        if reader.read_line(&mut first).is_err() {
            return;
        }
        // Drain headers.
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
            line.clear();
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = match (path, ready) {
        ("/health", true) => ("200 OK", r#"{"status":"ok"}"#),
        ("/health", false) => ("503 Service Unavailable", r#"{"status":"loading"}"#),
        _ => ("404 Not Found", r#"{"error":"not found"}"#),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

#[cfg(unix)]
fn spawn_sleeper(pid_file: Option<&str>) {
    match std::process::Command::new("sleep").arg("300").spawn() {
        Ok(child) => {
            eprintln!("fake-engine: sleeper pid {}", child.id());
            if let Some(path) = pid_file {
                let _ = std::fs::write(path, child.id().to_string());
            }
            // Deliberately not waited for: it must outlive us unless the group is killed.
            std::mem::forget(child);
        }
        Err(e) => eprintln!("fake-engine: cannot spawn sleeper: {e}"),
    }
}

#[cfg(not(unix))]
fn spawn_sleeper(_pid_file: Option<&str>) {}
