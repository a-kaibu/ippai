//! Runs' read traces (vfs/trace.rs, kept by ippai serve in `<jit>/traces/`) replayed in time
//! against a model of the bucket, for ways of foreseeing reads: how many first reads of a chunk
//! still waited, how long in all, and how many chunks were fetched ahead for nothing. Unlike
//! hints_eval.rs (orders of first reads, no times), a chunk hinted too late waits here too.
//!
//!   REPLAY_TRACES=<jit>/traces cargo test -p kura --lib replay_traces -- --ignored --nocapture
//!
//! The model: each request to the bucket answers after LATENCY_MS whatever its size (it does:
//! 0.4-2 s, docs/s3-reads.md); the game's own fetch goes at once, fetches ahead WIDTH at a time,
//! the newest hints first (host/fetch-bridge.js ippaiHint). The game's time between reads is
//! what the trace shows less what each read waited then. The device starts empty.
//!
//! Foresights, each from what the game read so far only:
//! - `layout`, `script`: the run's own hints of that source (sent right after the read that
//!   gave them: vfs/hints.rs, vfs/predict.rs);
//! - `next`: what followed the chunk in the game's other runs (fetch-bridge.js ippaiFollow);
//! - `list`: the other runs' first reads in order, behind every hint (the warm).

use super::trace::{parse, Line};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const LATENCY_MS: u64 = 1500;
pub const WIDTH: usize = 6;
const QUEUE_MAX: usize = 256;
const FOLLOW: usize = 12;

/// A run: its first reads (ms, chunk, waited ms) and hints (after read index, source, chunks).
pub struct Run {
    pub reads: Vec<(u64, String, u64)>,
    pub hints: Vec<(usize, String, Vec<String>)>,
}

pub fn run(text: &str) -> Run {
    let (mut reads, mut hints) = (Vec::new(), Vec::new());
    for l in parse(text) {
        match l {
            Line::Read { ms, hash, waited, .. } => reads.push((ms, hash, waited)),
            Line::Hint { source, hashes, .. } => hints.push((reads.len().saturating_sub(1), source, hashes)),
            Line::Path { .. } => {}
        }
    }
    Run { reads, hints }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Score {
    pub reads: u64,
    pub waited: u64,
    pub wait_ms: u64,
    pub ahead: u64,
    pub unused: u64,
}

/// What a foresight takes from: the game's other runs.
#[derive(Default)]
pub struct Others {
    pub next: HashMap<String, Vec<String>>,
    pub list: Vec<String>,
}

pub fn others(runs: &[&Run]) -> Others {
    let mut o = Others::default();
    let mut seen = HashSet::new();
    for r in runs {
        for w in r.reads.windows(2) {
            let v = o.next.entry(w[0].1.clone()).or_default();
            if !v.contains(&w[1].1) && v.len() < 4 {
                v.push(w[1].1.clone());
            }
        }
        for (_, h, _) in &r.reads {
            if seen.insert(h.clone()) {
                o.list.push(h.clone());
            }
        }
    }
    o
}

/// What `next` foresees after chunk `h`: breadth first, FOLLOW at most.
fn follow(next: &HashMap<String, Vec<String>>, h: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::from([h]);
    let mut level = vec![h];
    while !level.is_empty() && out.len() < FOLLOW {
        let mut n = Vec::new();
        for a in level {
            for b in next.get(a).into_iter().flatten() {
                if seen.insert(b) {
                    n.push(b.as_str());
                }
            }
        }
        out.extend(n.iter().map(|s| s.to_string()));
        level = n;
    }
    out.truncate(FOLLOW);
    out
}

/// `r` replayed with the foresights named in `use_` ("layout", "script", "next", "list").
pub fn replay(r: &Run, o: &Others, use_: &[&str]) -> Score {
    let mut s = Score::default();
    // chunk -> when it is on the device
    let mut at: HashMap<String, u64> = HashMap::new();
    let mut slots = vec![0u64; WIDTH];
    let mut queue: Vec<(String, u64)> = Vec::new();
    let mut list = if use_.contains(&"list") { o.list.clone() } else { Vec::new() };
    list.reverse();
    let mut ahead: HashSet<String> = HashSet::new();
    let read_set: HashSet<&str> = r.reads.iter().map(|x| x.1.as_str()).collect();
    // the fetches ahead that start by `to`
    let advance = |to: u64, at: &mut HashMap<String, u64>, slots: &mut Vec<u64>, queue: &mut Vec<(String, u64)>, list: &mut Vec<String>, ahead: &mut HashSet<String>| loop {
        let (k, free) = slots.iter().copied().enumerate().min_by_key(|x| x.1).unwrap();
        let next = loop {
            match queue.pop() {
                Some((h, t)) if !at.contains_key(&h) => break Some((h, t)),
                Some(_) => continue,
                None => match list.pop() {
                    Some(h) if !at.contains_key(&h) => break Some((h, 0)),
                    Some(_) => continue,
                    None => break None,
                },
            }
        };
        let Some((h, t)) = next else { break };
        let start = free.max(t);
        if start > to {
            queue.push((h, t));
            break;
        }
        slots[k] = start + LATENCY_MS;
        at.insert(h.clone(), start + LATENCY_MS);
        ahead.insert(h);
    };
    let mut clock = 0u64;
    let mut prev_end = 0u64;
    let mut hi = 0usize;
    for (i, (ms, h, waited)) in r.reads.iter().enumerate() {
        // (the game's time since its last read: as it was, less that read's wait)
        let gap = if i == 0 { 0 } else { ms.saturating_sub(prev_end).saturating_sub(*waited) };
        prev_end = *ms;
        clock += gap;
        advance(clock, &mut at, &mut slots, &mut queue, &mut list, &mut ahead);
        s.reads += 1;
        let wait = match at.get(h) {
            Some(&t) if t <= clock => 0,
            Some(&t) => t - clock,
            None => {
                at.insert(h.clone(), clock + LATENCY_MS);
                LATENCY_MS
            }
        };
        if wait > 0 {
            s.waited += 1;
            s.wait_ms += wait;
        }
        clock += wait;
        // what this read foresees
        let mut batch: Vec<String> = Vec::new();
        while hi < r.hints.len() && r.hints[hi].0 <= i {
            if use_.contains(&r.hints[hi].1.as_str()) {
                batch.extend(r.hints[hi].2.iter().cloned());
            }
            hi += 1;
        }
        if use_.contains(&"next") {
            batch.extend(follow(&o.next, h));
        }
        // (newest first, the batch's own first one first: pushed reversed)
        for x in batch.into_iter().rev() {
            if !at.contains_key(&x) {
                queue.push((x, clock));
            }
        }
        if queue.len() > QUEUE_MAX {
            queue.drain(..queue.len() - QUEUE_MAX);
        }
    }
    s.ahead = ahead.len() as u64;
    s.unused = ahead.iter().filter(|h| !read_set.contains(h.as_str())).count() as u64;
    s
}

pub const VARIANTS: [(&str, &[&str]); 7] = [
    ("none", &[]),
    ("layout", &["layout"]),
    ("script", &["script"]),
    ("layout+script", &["layout", "script"]),
    ("next", &["next"]),
    ("layout+script+next", &["layout", "script", "next"]),
    ("all+list", &["layout", "script", "next", "list"]),
];

/// Per game (the trace's name: `trace.<game>.<run>`), each variant's scores summed over its
/// runs, each run against its game's other runs.
pub fn by_game(traces: &[(String, String)]) -> BTreeMap<String, Vec<Score>> {
    let mut games: BTreeMap<String, Vec<Run>> = BTreeMap::new();
    for (name, text) in traces {
        let game = name.strip_prefix("trace.").and_then(|n| n.rsplit_once('.')).map_or(name.as_str(), |x| x.0);
        games.entry(game.to_string()).or_default().push(run(text));
    }
    let mut out = BTreeMap::new();
    for (g, runs) in games {
        let mut sums = vec![Score::default(); VARIANTS.len()];
        for (i, r) in runs.iter().enumerate() {
            let rest: Vec<&Run> = runs.iter().enumerate().filter(|x| x.0 != i).map(|x| x.1).collect();
            let o = others(&rest);
            for (k, (_, use_)) in VARIANTS.iter().enumerate() {
                let x = replay(r, &o, use_);
                let t = &mut sums[k];
                t.reads += x.reads;
                t.waited += x.waited;
                t.wait_ms += x.wait_ms;
                t.ahead += x.ahead;
                t.unused += x.unused;
            }
        }
        out.insert(g, sums);
    }
    out
}

#[test]
#[ignore]
fn replay_traces() {
    let Ok(dir) = std::env::var("REPLAY_TRACES") else {
        eprintln!("set REPLAY_TRACES");
        return;
    };
    let mut traces = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("trace.") && !n.ends_with(".tmp") {
            traces.push((n, std::fs::read_to_string(e.path()).unwrap_or_default()));
        }
    }
    println!("game\tvariant\treads\twaited\twaited%\twait s\tahead\tunused");
    let mut total = vec![Score::default(); VARIANTS.len()];
    for (g, sums) in by_game(&traces) {
        for (k, s) in sums.iter().enumerate() {
            println!("{g}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{}\t{}", VARIANTS[k].0, s.reads, s.waited, 100.0 * s.waited as f64 / s.reads.max(1) as f64, s.wait_ms as f64 / 1000.0, s.ahead, s.unused);
            let t = &mut total[k];
            t.reads += s.reads;
            t.waited += s.waited;
            t.wait_ms += s.wait_ms;
            t.ahead += s.ahead;
            t.unused += s.unused;
        }
    }
    for (k, s) in total.iter().enumerate() {
        println!("ALL\t{}\t{}\t{}\t{:.1}\t{:.1}\t{}\t{}", VARIANTS[k].0, s.reads, s.waited, 100.0 * s.waited as f64 / s.reads.max(1) as f64, s.wait_ms as f64 / 1000.0, s.ahead, s.unused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(lines: &[&str]) -> String {
        format!("ippai-trace 1\n{}\n", lines.join("\n"))
    }

    #[test]
    fn a_read_alone_waits_one_request() {
        let r = run(&trace(&["R\t0\t1\t0\t0\taa\t1500", "R\t1600\t1\t0\t1\tbb\t1500"]));
        let s = replay(&r, &Others::default(), &[]);
        assert_eq!(s, Score { reads: 2, waited: 2, wait_ms: 3000, ahead: 0, unused: 0 });
    }

    /// A hint in time saves the wait; one too late saves part of it; one for nothing counts.
    #[test]
    fn hints_in_time_and_late() {
        // the game reads aa, thinks 2 s (the trace: 1.5 s waited + 2 s), reads bb, at once cc
        let r = run(&trace(&[
            "R\t1500\t1\t0\t0\taa\t1500",
            "H\t1500\tlayout\tbb,zz",
            "R\t5000\t1\t0\t1\tbb\t1500",
            "H\t5000\tscript\tcc",
            "R\t5000\t1\t0\t2\tcc\t0",
        ]));
        let none = replay(&r, &Others::default(), &[]);
        assert_eq!((none.waited, none.wait_ms), (3, 4500));
        let layout = replay(&r, &Others::default(), &["layout"]);
        // (bb in time; cc not hinted)
        assert_eq!((layout.waited, layout.wait_ms, layout.ahead, layout.unused), (2, 3000, 2, 1));
        let both = replay(&r, &Others::default(), &["layout", "script"]);
        // (cc hinted when bb was read, then read at once: waits all of it, the hint's fetch)
        assert_eq!((both.waited, both.wait_ms), (2, 3000));
    }

    /// Other runs' next chunks: the way the game goes now, of the ways it went before.
    #[test]
    fn next_follows_the_branch_taken() {
        let story = run(&trace(&["R\t0\t1\t0\t0\ttitle\t0", "R\t3000\t1\t0\t0\ts1\t0", "R\t6000\t1\t0\t0\ts2\t0"]));
        let gallery = run(&trace(&["R\t0\t1\t0\t0\ttitle\t0", "R\t3000\t1\t0\t0\tg1\t0", "R\t6000\t1\t0\t0\tg2\t0"]));
        let o = others(&[&story]);
        assert_eq!(follow(&o.next, "title"), ["s1", "s2"]);
        let both = others(&[&story, &gallery]);
        assert_eq!(follow(&both.next, "title"), ["s1", "g1", "s2", "g2"]);
        // this run goes to the gallery: with both runs known, only the title waits
        let s = replay(&gallery, &both, &["next"]);
        assert_eq!(s.waited, 1);
        assert_eq!(s.unused, 2, "the story's two fetched for nothing");
        let by = by_game(&[("trace.game.a.1".into(), trace(&["R\t0\t1\t0\t0\ttitle\t0", "R\t3000\t1\t0\t0\tg1\t0"])), ("trace.game.a.2".into(), trace(&["R\t0\t1\t0\t0\ttitle\t0", "R\t3000\t1\t0\t0\tg1\t0"]))]);
        let a = &by["game.a"];
        assert_eq!(a[0].waited, 4, "none: every read of both runs");
        assert_eq!(a[4].waited, 2, "next: the title of each");
    }

    /// The list: behind the hints, fetched while the game thinks.
    #[test]
    fn the_list_fills_idle_time() {
        let earlier = run(&trace(&["R\t0\t1\t0\t0\ta\t0", "R\t1\t1\t0\t0\tb\t0", "R\t2\t1\t0\t0\tc\t0"]));
        let now = run(&trace(&["R\t0\t1\t0\t0\ta\t0", "R\t9000\t1\t0\t0\tc\t0"]));
        let o = others(&[&earlier]);
        assert_eq!(replay(&now, &o, &[]).waited, 2);
        let s = replay(&now, &o, &["list"]);
        // (a was on its way with the game's first read, c came while the game thought)
        assert_eq!((s.waited, s.wait_ms), (1, 1500));
        assert_eq!(s.unused, 1);
    }
}
