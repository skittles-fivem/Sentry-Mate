//! `cargo run -p logfmt --example candump -- LOG.log.xz > LOG.log`: a log of either format as candump text.
use std::io::Write;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: candump LOG.log.xz");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    let result = logfmt::read_log(&bytes, logfmt::Limits::default(), |l| {
        let _ = out.write_all(l);
        let _ = out.write_all(b"\n");
    });
    let _ = out.flush();
    match result {
        Ok(s) => eprintln!(
            "{}: {} lines, {} long lines skipped{}",
            s.format.name(),
            s.lines,
            s.skipped_long_lines,
            if s.truncated {
                ", cut at the limit"
            } else {
                ""
            }
        ),
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    }
}
