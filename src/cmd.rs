use std::{
    collections::BTreeMap,
    io::{BufRead as _, BufReader, Read, Write},
    process::{Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
};

use camino::Utf8PathBuf;
use secrecy::{ExposeSecret, SecretString};

#[derive(Debug)]
pub struct CmdOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl CmdOutput {
    pub fn status(&self) -> &ExitStatus {
        &self.status
    }

    pub fn stdout(&self) -> &str {
        self.stdout.trim()
    }

    pub fn stderr(&self) -> &str {
        self.stderr.trim()
    }
}

pub struct Cmd {
    name: String,
    env_vars: BTreeMap<String, SecretString>,
    args: Vec<String>,
    current_dir: Option<Utf8PathBuf>,
    hide_stdout: bool,
    hide_stderr: bool,
    hide_command: bool,
}

impl Cmd {
    pub fn new<I, S>(cmd_name: &str, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args: Vec<String> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect();
        Self {
            name: cmd_name.to_string(),
            args,
            current_dir: None,
            hide_stdout: false,
            hide_stderr: false,
            hide_command: false,
            env_vars: BTreeMap::new(),
        }
    }

    pub fn with_env_vars(&mut self, env_vars: BTreeMap<String, SecretString>) -> &mut Self {
        self.env_vars = env_vars;
        self
    }

    pub fn with_current_dir(&mut self, dir: impl Into<Utf8PathBuf>) -> &mut Self {
        self.current_dir = Some(dir.into());
        self
    }

    pub fn hide_stdout(&mut self) -> &mut Self {
        self.hide_stdout = true;
        self
    }

    pub fn hide_stderr(&mut self) -> &mut Self {
        self.hide_stderr = true;
        self
    }

    pub fn hide_command(&mut self) -> &mut Self {
        self.hide_command = true;
        self
    }

    pub fn run(&self) -> CmdOutput {
        let mut command = self.command();
        self.print_command();
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let (tx, rx) = mpsc::channel();

        // Thread to read stdout
        let tx_clone = tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = line.unwrap();
                tx_clone.send((line.clone(), true)).unwrap();
            }
        });

        // Thread to read stderr
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let line = line.unwrap();
                tx.send((line.clone(), false)).unwrap();
            }
        });

        let mut output_stdout = String::new();
        let mut output_stderr = String::new();

        for (line, is_stdout) in rx {
            if is_stdout {
                if !self.hide_stdout {
                    println!("{line}");
                }
                output_stdout.push_str(&line);
                output_stdout.push('\n');
            } else {
                if !self.hide_stderr {
                    eprintln!("{line}");
                }
                output_stderr.push_str(&line);
                output_stderr.push('\n');
            }
        }
        let output = child.wait().unwrap();

        CmdOutput {
            status: output,
            stdout: output_stdout,
            stderr: output_stderr,
        }
    }

    /// Run a command attached to the terminal.
    ///
    /// Unlike [`Self::run`], this preserves interactive prompts such as the
    /// confirmation requested by `terraform apply`.
    pub fn run_interactive(&self) -> ExitStatus {
        assert!(
            !self.hide_stdout && !self.hide_stderr,
            "interactive commands inherit stdout and stderr and cannot hide them"
        );
        let mut command = self.command();
        self.print_command();

        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .unwrap()
    }

    /// Run a command attached to the terminal while capturing its output.
    ///
    /// Output is forwarded as bytes instead of lines so prompts that do not end
    /// in a newline remain visible to the user.
    pub fn run_interactive_with_output(&self) -> CmdOutput {
        assert!(
            !self.hide_stdout && !self.hide_stderr,
            "interactive commands inherit stdout and stderr and cannot hide them"
        );
        let mut command = self.command();
        self.print_command();
        let mut child = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stdout_thread = thread::spawn(move || capture_and_forward(stdout, std::io::stdout()));
        let stderr_thread = thread::spawn(move || capture_and_forward(stderr, std::io::stderr()));

        let status = child.wait().unwrap();
        let stdout = stdout_thread.join().unwrap();
        let stderr = stderr_thread.join().unwrap();

        CmdOutput {
            status,
            stdout,
            stderr,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.name);
        command.args(&self.args);
        if let Some(dir) = &self.current_dir {
            command.current_dir(dir);
        }
        for (key, value) in &self.env_vars {
            command.env(key, value.expose_secret());
        }
        command
    }

    fn print_command(&self) {
        if self.hide_command {
            return;
        }

        let mut command = format!("🚀 {} {}", self.name, self.args.join(" "));
        if let Some(dir) = &self.current_dir {
            command.push_str(&format!(" 👉 {dir}"));
        }
        println!("{command}");
    }
}

fn capture_and_forward(mut reader: impl Read, mut writer: impl Write) -> String {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = reader.read(&mut buffer).unwrap();
        if bytes_read == 0 {
            break;
        }
        writer.write_all(&buffer[..bytes_read]).unwrap();
        writer.flush().unwrap();
        output.extend_from_slice(&buffer[..bytes_read]);
    }
    String::from_utf8_lossy(&output).into_owned()
}
