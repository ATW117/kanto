//! Driving the Node Showdown runner from Rust: N worker processes fed JSON-lines specs.

use crate::spec::Spec;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

pub struct Showdown {
    pub node: PathBuf,
    pub ps_dir: PathBuf,
    pub runner: PathBuf,
}

impl Showdown {
    /// Locate node and pokemon-showdown: `NODE`/`PS_DIR` env vars, else `node` on PATH /
    /// `~/.nvm/versions/node/*/bin/node`.
    pub fn locate(repo: &Path, ps_dir: Option<&str>) -> Result<Showdown, String> {
        let node = if let Ok(n) = std::env::var("NODE") {
            PathBuf::from(n)
        } else if Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            PathBuf::from("node")
        } else {
            let home = std::env::var("HOME").unwrap_or_default();
            let dir = Path::new(&home).join(".nvm/versions/node");
            let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map(|it| it.flatten().map(|e| e.path().join("bin/node")).filter(|p| p.exists()).collect())
                .unwrap_or_default();
            found.sort();
            found.pop().ok_or("node not found: set NODE=/path/to/node")?
        };
        let ps_dir = ps_dir
            .map(PathBuf::from)
            .or_else(|| std::env::var("PS_DIR").ok().map(PathBuf::from))
            .ok_or("pokemon-showdown not found: set PS_DIR=/path/to/node_modules/pokemon-showdown")?;
        if !ps_dir.join("dist/sim").exists() {
            return Err(format!("{} is not a built pokemon-showdown package", ps_dir.display()));
        }
        Ok(Showdown { node, ps_dir, runner: repo.join("parity/showdown/runner.js") })
    }

    /// Run `specs` on `jobs` worker processes; `on_trace` receives each output line (in completion
    /// order) together with its spec index.
    pub fn run(&self, specs: &[Spec], jobs: usize, mut on_trace: impl FnMut(usize, String)) -> Result<(), String> {
        let jobs = jobs.max(1).min(specs.len().max(1));
        let (tx, rx) = mpsc::channel::<(usize, String)>();
        let mut handles = Vec::new();
        for w in 0..jobs {
            let idx: Vec<usize> = (w..specs.len()).step_by(jobs).collect();
            if idx.is_empty() {
                continue;
            }
            let mut child = Command::new(&self.node)
                .arg(&self.runner)
                .env("PS_DIR", &self.ps_dir)
                .env("PS_PATCH", specs.first().map(|s| s.patch.as_str()).unwrap_or("vanilla"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| format!("spawning {}: {e}", self.node.display()))?;
            let mut stdin = child.stdin.take().unwrap();
            let lines: Vec<String> = idx.iter().map(|&i| serde_json::to_string(&specs[i]).unwrap()).collect();
            let writer = std::thread::spawn(move || {
                for l in lines {
                    if writeln!(stdin, "{l}").is_err() {
                        break;
                    }
                }
            });
            let stdout = child.stdout.take().unwrap();
            let tx = tx.clone();
            let reader = std::thread::spawn(move || {
                let r = BufReader::with_capacity(1 << 20, stdout);
                for (k, line) in r.lines().enumerate() {
                    let Ok(line) = line else { break };
                    if tx.send((idx[k], line)).is_err() {
                        break;
                    }
                }
                let _ = child.wait();
            });
            handles.push((writer, reader));
        }
        drop(tx);
        for (i, line) in rx {
            on_trace(i, line);
        }
        for (w, r) in handles {
            let _ = w.join();
            let _ = r.join();
        }
        Ok(())
    }
}
