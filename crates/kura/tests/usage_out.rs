//! MASU_API_COUNTS_OUT (tests/coverage/usage.py's tests side): each process appends its tally
//! as one line at exit and prints none on stderr (host/tests and the Wine tests compare their
//! output); without it the tally goes to stderr, at exit too, as the corpus' logs have it.

use std::process::Command;

#[test]
fn tallies_append_a_line_per_process() {
    if let Some(n) = std::env::var_os("USAGE_OUT_CHILD") {
        let n: u64 = n.to_str().unwrap().parse().unwrap();
        // (past 4096: where a line is printed on stderr without the file)
        for _ in 0..5000 + n {
            kura::usage::named("api", "k32!Tested");
        }
        kura::usage::named("api", &format!("k32!Only{n}"));
        std::process::exit(0);
    }
    let dir = std::env::temp_dir().join(format!("usage-out-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("tallies.txt");
    let _ = std::fs::remove_file(&out);
    let exe = std::env::current_exe().unwrap();
    let child = |n: u64, file: bool| {
        let mut c = Command::new(&exe);
        c.args(["--exact", "tallies_append_a_line_per_process", "--nocapture"]).env("USAGE_OUT_CHILD", n.to_string()).env("MASU_API_COUNTS", "1");
        if file {
            c.env("MASU_API_COUNTS_OUT", &out);
        } else {
            c.env_remove("MASU_API_COUNTS_OUT");
        }
        let o = c.output().unwrap();
        assert!(o.status.success());
        String::from_utf8(o.stderr).unwrap()
    };
    for n in 0..3 {
        let err = child(n, true);
        assert!(!err.contains("api-counts"), "a tally on stderr: {err}");
    }
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    for (n, l) in lines.iter().enumerate() {
        assert_eq!(*l, format!("api-counts api:k32!Only{n}=1 api:k32!Tested={}", 5000 + n));
    }
    // without the file: on stderr, the last line the whole tally
    let err = child(7, false);
    let tallies: Vec<&str> = err.lines().filter(|l| l.starts_with("api-counts")).collect();
    assert_eq!(*tallies.last().unwrap(), "api-counts api:k32!Only7=1 api:k32!Tested=5007");
    assert!(tallies.len() >= 2, "the line at 4096 too");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
    let _ = std::fs::remove_dir_all(dir);
}
