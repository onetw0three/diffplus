//! Bounded, live subprocess output for long-running external analyzers.

use anyhow::{bail, Context, Result};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const TAIL_LIMIT: usize = 64 * 1024;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) fn run(command: &mut Command, label: &str) -> Result<()> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {label}"))?;
    let stdout = child.stdout.take().context("capturing subprocess stdout")?;
    let stderr = child.stderr.take().context("capturing subprocess stderr")?;
    let stdout_reader = spawn_reader(stdout, label.to_owned(), "stdout");
    let stderr_reader = spawn_reader(stderr, label.to_owned(), "stderr");

    let started = Instant::now();
    let mut next_heartbeat = HEARTBEAT_INTERVAL;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= next_heartbeat {
            crate::progress::info(format!(
                "{label} still running ({}s elapsed)",
                started.elapsed().as_secs()
            ));
            next_heartbeat += HEARTBEAT_INTERVAL;
        }
        thread::sleep(POLL_INTERVAL);
    };

    let stdout = join_reader(stdout_reader, label)?;
    let stderr = join_reader(stderr_reader, label)?;
    check_status(label, status, &stdout, &stderr)
}

fn spawn_reader<R>(
    source: R,
    label: String,
    stream: &'static str,
) -> thread::JoinHandle<Result<Vec<u8>, std::io::Error>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(source);
        let mut tail = VecDeque::with_capacity(TAIL_LIMIT);
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let display = String::from_utf8_lossy(&line);
            let display = display.trim_end_matches(['\r', '\n']);
            if !display.is_empty() {
                crate::progress::subprocess(&label, stream, display);
            }
            for byte in &line {
                if tail.len() == TAIL_LIMIT {
                    tail.pop_front();
                }
                tail.push_back(*byte);
            }
        }
        Ok(tail.into_iter().collect())
    })
}

fn join_reader(
    reader: thread::JoinHandle<Result<Vec<u8>, std::io::Error>>,
    label: &str,
) -> Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("{label} output reader panicked"))?
        .with_context(|| format!("reading {label} output"))
}

fn check_status(label: &str, status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    bail!(
        "{label} failed with {status}: {}{}{}",
        stdout.trim(),
        if stdout.is_empty() || stderr.is_empty() {
            ""
        } else {
            "\n"
        },
        stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn accepts_success_and_reports_failure_tail() {
        run(
            Command::new("sh").args(["-c", "echo ready; echo detail >&2"]),
            "mock",
        )
        .unwrap();
        let error = run(
            Command::new("sh").args(["-c", "echo broken >&2; exit 7"]),
            "mock",
        )
        .unwrap_err();
        assert!(error.to_string().contains("broken"));
        assert!(error.to_string().contains('7'));
    }
}
