//! `kanto-simulate`: a drop-in for `pokemon-showdown simulate-battle` (`[Gen 1] OU`).
//!
//! Reads Showdown's battle-stream protocol on stdin (`>start {...}`, `>player p1 {...}`,
//! `>p1 move 1`, ...) and writes what Showdown's `BattleTextStream` writes to stdout: `update`,
//! `sideupdate`, `end` and `requesteddata` messages, each followed by a blank line. Input is
//! processed in the chunks it arrives in (as Node does), so an interactive client that writes
//! one command at a time gets one response per command. See `kanto_backend::sd`.
use kanto_backend::sd::BattleTextStream;
use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(a) = args.iter().find(|a| a.as_str() != "simulate-battle" && a.as_str() != "--skip-build") {
        eprintln!("kanto-simulate: unsupported option {a} (the --debug/--replay/--spectate modes are not implemented)");
        std::process::exit(2);
    }
    let mut stream = BattleTextStream::new();
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut buf = vec![0u8; 65536];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                eprintln!("kanto-simulate: {e}");
                std::process::exit(1);
            }
        };
        pending.extend_from_slice(&buf[..n]);
        // Keep an incomplete UTF-8 sequence for the next read.
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => pending.len(),
        };
        let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
        pending.drain(..valid);
        match stream.write(&text) {
            Ok(out) => {
                if !out.is_empty() {
                    let _ = stdout.write_all(out.as_bytes());
                    let _ = stdout.flush();
                }
            }
            Err(e) => {
                let _ = stdout.write_all(e.output.as_bytes());
                let _ = stdout.flush();
                eprintln!("{}", e.message);
                std::process::exit(1);
            }
        }
    }
}
