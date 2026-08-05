use crate::pbs::CommandSpec;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, BufReader};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandEnvironment {
    variables: BTreeMap<String, String>,
}

impl CommandEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.variables.insert(name.into(), value.into());
    }

    pub fn is_empty(&self) -> bool {
        self.variables.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status.success()
    }

    pub fn combined_log(&self) -> String {
        match (self.stdout.trim(), self.stderr.trim()) {
            ("", "") => String::new(),
            (stdout, "") => stdout.to_owned(),
            ("", stderr) => stderr.to_owned(),
            (stdout, stderr) => format!("{stdout}\n{stderr}"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    canceled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
    }

    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub enum ExecutorError {
    Spawn { program: String, source: io::Error },
    Canceled { elapsed: Duration },
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, source } => {
                write!(formatter, "failed to run {program}: {source}")
            }
            Self::Canceled { elapsed } => {
                write!(formatter, "canceled after {:.1}s", elapsed.as_secs_f32())
            }
        }
    }
}

impl Error for ExecutorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            Self::Canceled { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

pub fn run_command(
    spec: &CommandSpec,
    environment: &CommandEnvironment,
) -> Result<CommandOutput, ExecutorError> {
    let mut stdout = String::new();
    let mut stderr = String::new();
    run_command_streaming(spec, environment, |stream, line| match stream {
        OutputStream::Stdout => {
            stdout.push_str(line);
            stdout.push('\n');
        }
        OutputStream::Stderr => {
            stderr.push_str(line);
            stderr.push('\n');
        }
    })
    .map(|mut output| {
        output.stdout = stdout;
        output.stderr = stderr;
        output
    })
}

pub fn run_command_streaming(
    spec: &CommandSpec,
    environment: &CommandEnvironment,
    on_line: impl FnMut(OutputStream, &str),
) -> Result<CommandOutput, ExecutorError> {
    run_command_streaming_cancelable(spec, environment, &CancellationToken::new(), on_line)
}

pub fn run_command_streaming_cancelable(
    spec: &CommandSpec,
    environment: &CommandEnvironment,
    cancellation: &CancellationToken,
    mut on_line: impl FnMut(OutputStream, &str),
) -> Result<CommandOutput, ExecutorError> {
    let started = Instant::now();
    let mut child = Command::new(&spec.program)
        .args(&spec.arguments)
        .envs(environment.variables.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| ExecutorError::Spawn {
            program: spec.program.to_string_lossy().into_owned(),
            source,
        })?;

    let stdout = child.stdout.take().expect("stdout was configured as piped");
    let stderr = child.stderr.take().expect("stderr was configured as piped");
    let (sender, receiver) = mpsc::channel();

    spawn_reader(OutputStream::Stdout, stdout, sender.clone());
    spawn_reader(OutputStream::Stderr, stderr, sender);

    let mut stdout = String::new();
    let mut stderr = String::new();
    let status = loop {
        drain_lines(&receiver, &mut stdout, &mut stderr, &mut on_line);
        if cancellation.is_canceled() {
            let _ = child.kill();
            let _ = child.wait();
            drain_lines(&receiver, &mut stdout, &mut stderr, &mut on_line);
            return Err(ExecutorError::Canceled {
                elapsed: started.elapsed(),
            });
        }
        if let Some(status) = child.try_wait().map_err(|source| ExecutorError::Spawn {
            program: spec.program.to_string_lossy().into_owned(),
            source,
        })? {
            break status;
        }
        thread::sleep(Duration::from_millis(50));
    };

    drop(child);
    for (stream, line) in receiver {
        push_line(stream, &line, &mut stdout, &mut stderr, &mut on_line);
    }

    Ok(CommandOutput {
        status,
        stdout,
        stderr,
        elapsed: started.elapsed(),
    })
}

fn drain_lines(
    receiver: &mpsc::Receiver<(OutputStream, String)>,
    stdout: &mut String,
    stderr: &mut String,
    on_line: &mut impl FnMut(OutputStream, &str),
) {
    while let Ok((stream, line)) = receiver.try_recv() {
        push_line(stream, &line, stdout, stderr, on_line);
    }
}

fn push_line(
    stream: OutputStream,
    line: &str,
    stdout: &mut String,
    stderr: &mut String,
    on_line: &mut impl FnMut(OutputStream, &str),
) {
    on_line(stream, line);
    match stream {
        OutputStream::Stdout => {
            stdout.push_str(line);
            stdout.push('\n');
        }
        OutputStream::Stderr => {
            stderr.push_str(line);
            stderr.push('\n');
        }
    }
}

fn spawn_reader(
    stream: OutputStream,
    output: impl io::Read + Send + 'static,
    sender: mpsc::Sender<(OutputStream, String)>,
) {
    thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            let _ = sender.send((stream, line));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pbs::CommandSpec;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn runs_command_without_shell() {
        let spec = CommandSpec {
            program: PathBuf::from("printf"),
            arguments: vec![OsString::from("hello")],
            required_environment: Vec::new(),
        };
        let output = run_command(&spec, &CommandEnvironment::new()).expect("run command");
        assert!(output.success());
        assert_eq!(output.stdout.trim_end(), "hello");
    }

    #[test]
    fn streams_output_lines() {
        let spec = CommandSpec {
            program: PathBuf::from("printf"),
            arguments: vec![OsString::from("a\nb\n")],
            required_environment: Vec::new(),
        };
        let mut lines = Vec::new();
        let output = run_command_streaming(&spec, &CommandEnvironment::new(), |_, line| {
            lines.push(line.to_owned())
        })
        .expect("run command");
        assert!(output.success());
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn injects_environment_variables() {
        let spec = CommandSpec {
            program: PathBuf::from("sh"),
            arguments: vec![
                OsString::from("-c"),
                OsString::from("printf %s \"$CRUMBS_TEST_VALUE\""),
            ],
            required_environment: Vec::new(),
        };
        let mut environment = CommandEnvironment::new();
        environment.insert("CRUMBS_TEST_VALUE", "works");
        let output = run_command(&spec, &environment).expect("run command");
        assert!(output.success());
        assert_eq!(output.stdout.trim_end(), "works");
    }
}
