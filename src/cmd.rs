use std::{
    collections::BTreeMap,
    io::{BufRead as _, BufReader},
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

    pub fn run(&self) -> CmdOutput {
        let mut to_print = format!("🚀 {} {}", self.name, self.args.join(" "));
        let mut command = Command::new(&self.name);
        if let Some(dir) = &self.current_dir {
            command.current_dir(dir);
            to_print.push_str(&format!(" 👉 {dir}"));
        }
        for (key, value) in &self.env_vars {
            command.env(key, value.expose_secret());
        }
        println!("{to_print}");
        let mut child = command
            .args(&self.args)
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
                eprintln!("{line}");
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
}
