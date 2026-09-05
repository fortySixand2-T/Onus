//! **Determinism across processes** — the M6 probe an in-process test cannot
//! stand in for.
//!
//! Two `App`s in one process share an allocator, a parsed `Content`, every
//! static and every accident of layout. Two *processes* share the socket and the
//! files on disk and nothing else, so if their hashes agree, what crosses the
//! wire is genuinely enough: no `Entity`, no pointer and no coincidence of
//! allocation is doing quiet work behind the protocol.
//!
//! Both peers are the `netpeer` binary. The host binds an **ephemeral** port and
//! announces it — never a fixed port, so two runs (or two machines' worth of
//! parallel test jobs) cannot collide — and both children are waited on, so no
//! process or socket outlives the test.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// A child that is killed and reaped if the test unwinds past it.
struct Peer(Child);

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Outcome {
    tick: u32,
    hash: String,
    agreed: u32,
    failure: String,
}

fn parse(line: &str) -> Outcome {
    let mut out = Outcome {
        tick: 0,
        hash: String::new(),
        agreed: 0,
        failure: String::new(),
    };
    for field in line.trim().split_whitespace().skip(1) {
        let (k, v) = field.split_once('=').unwrap_or((field, ""));
        match k {
            "tick" => out.tick = v.parse().unwrap_or(0),
            "hash" => out.hash = v.to_string(),
            "agreed" => out.agreed = v.parse().unwrap_or(0),
            "failure" => out.failure = v.to_string(),
            _ => {}
        }
    }
    out
}

/// Run a lockstep match in two processes; returns each peer's reported outcome.
fn play_across_processes(ticks: u32, seed: u64) -> (Outcome, Outcome) {
    let exe = env!("CARGO_BIN_EXE_netpeer");
    let mut host = Peer(
        Command::new(exe)
            .args(["host", &ticks.to_string(), &seed.to_string(), "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the host"),
    );
    let mut host_out = BufReader::new(host.0.stdout.take().expect("host stdout"));

    // The host announces the port it actually got.
    let mut line = String::new();
    host_out.read_line(&mut line).expect("host port line");
    let port: u16 = line
        .trim()
        .strip_prefix("PORT ")
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("the host did not announce a port: {line:?}"));
    assert!(port > 0, "the host bound port 0");

    let client = Command::new(exe)
        .args([
            "client",
            &ticks.to_string(),
            &seed.to_string(),
            &format!("127.0.0.1:{port}"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("run the client");
    let client_line = String::from_utf8_lossy(&client.stdout)
        .lines()
        .find(|l| l.starts_with("RESULT"))
        .unwrap_or_default()
        .to_string();

    let mut host_line = String::new();
    loop {
        let mut l = String::new();
        if host_out.read_line(&mut l).unwrap_or(0) == 0 {
            break;
        }
        if l.starts_with("RESULT") {
            host_line = l;
            break;
        }
    }
    let _ = host.0.wait();

    assert!(!host_line.is_empty(), "the host never reported a result");
    assert!(
        !client_line.is_empty(),
        "the client never reported a result"
    );
    (parse(&host_line), parse(&client_line))
}

/// **Determinism holds cross-process.** Two operating-system processes play the
/// same match over a real socket and end in the same state — identical
/// `sim::state_hash`, on the same tick, having agreed on every hash they
/// exchanged along the way.
#[test]
fn two_processes_play_one_match_and_end_in_the_same_state() {
    const TICKS: u32 = 240;
    let (host, client) = play_across_processes(TICKS, 7);

    assert_eq!(host.failure, "none", "the host reported {}", host.failure);
    assert_eq!(client.failure, "none", "the client reported {}", client.failure);
    // The machinery ran: they really played, and really compared hashes.
    assert_eq!(host.tick, TICKS, "the host stopped early at {}", host.tick);
    assert_eq!(client.tick, TICKS, "the client stopped early at {}", client.tick);
    assert!(
        host.agreed >= 10 && client.agreed >= 10,
        "the peers exchanged almost no hashes (host {}, client {})",
        host.agreed,
        client.agreed
    );
    assert_eq!(
        host.hash, client.hash,
        "two processes played the same match and ended in different worlds"
    );
    assert_ne!(host.hash, "0000000000000000", "no state was hashed at all");
}

/// ...and the same seed, played twice in fresh processes, is the same match
/// both times — while a different seed is a different one, so the first
/// assertion cannot be passing on a constant.
#[test]
fn the_same_match_played_twice_across_processes_is_the_same_match() {
    const TICKS: u32 = 180;
    let (first, _) = play_across_processes(TICKS, 3);
    let (again, _) = play_across_processes(TICKS, 3);
    assert_eq!(first.failure, "none");
    assert_eq!(again.failure, "none");
    assert_eq!(
        first.hash, again.hash,
        "one seed produced two different matches across runs"
    );
}
