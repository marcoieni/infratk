use std::{
    collections::BTreeMap,
    io::{BufRead as _, BufReader, Read, Write},
    process::{Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
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
    output_notification: Option<OutputNotification>,
}

#[derive(Clone)]
struct OutputNotification {
    trigger: String,
    title: String,
    message: String,
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
            output_notification: None,
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

    pub fn notify_on_output(
        &mut self,
        trigger: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> &mut Self {
        let trigger = trigger.into();
        assert!(!trigger.is_empty(), "notification trigger cannot be empty");
        self.output_notification = Some(OutputNotification {
            trigger,
            title: title.into(),
            message: message.into(),
        });
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
        let notified = Arc::new(AtomicBool::new(false));
        let stdout_notification = self.output_notification.clone();
        let stderr_notification = self.output_notification.clone();
        let stdout_notified = Arc::clone(&notified);
        let stderr_notified = Arc::clone(&notified);
        let stdout_thread = thread::spawn(move || {
            capture_and_forward(
                stdout,
                std::io::stdout(),
                stdout_notification.as_ref(),
                &stdout_notified,
            )
        });
        let stderr_thread = thread::spawn(move || {
            capture_and_forward(
                stderr,
                std::io::stderr(),
                stderr_notification.as_ref(),
                &stderr_notified,
            )
        });

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

fn capture_and_forward(
    mut reader: impl Read,
    mut writer: impl Write,
    notification: Option<&OutputNotification>,
    notified: &AtomicBool,
) -> String {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut matcher = notification
        .as_ref()
        .map(|notification| OutputMatcher::new(notification.trigger.as_bytes()));
    loop {
        let bytes_read = reader.read(&mut buffer).unwrap();
        if bytes_read == 0 {
            break;
        }
        let bytes = &buffer[..bytes_read];
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
        output.extend_from_slice(bytes);

        let trigger_seen = matcher
            .as_mut()
            .is_some_and(|matcher| matcher.observe(bytes));
        if trigger_seen && !notified.swap(true, Ordering::Relaxed) {
            let notification = notification.as_ref().unwrap();
            writer
                .write_all(
                    osc777_notification(&notification.title, &notification.message).as_bytes(),
                )
                .unwrap();
            writer.flush().unwrap();
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

struct OutputMatcher {
    trigger: Vec<u8>,
    pending: Vec<u8>,
}

impl OutputMatcher {
    fn new(trigger: &[u8]) -> Self {
        assert!(!trigger.is_empty());
        Self {
            trigger: trigger.to_vec(),
            pending: Vec::with_capacity(trigger.len() * 2),
        }
    }

    fn observe(&mut self, bytes: &[u8]) -> bool {
        self.pending.extend_from_slice(bytes);
        if self
            .pending
            .windows(self.trigger.len())
            .any(|window| window == self.trigger)
        {
            return true;
        }

        let bytes_to_keep = self.trigger.len().saturating_sub(1);
        if self.pending.len() > bytes_to_keep {
            self.pending
                .drain(..self.pending.len().saturating_sub(bytes_to_keep));
        }
        false
    }
}

fn osc777_notification(title: &str, message: &str) -> String {
    format!(
        "\x1b]777;notify;{};{}\x07",
        sanitize_notification_field(title),
        sanitize_notification_field(message)
    )
}

fn sanitize_notification_field(field: &str) -> String {
    field
        .chars()
        .map(|character| match character {
            ';' => ':',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_matcher_detects_a_trigger_split_across_reads() {
        let mut matcher = OutputMatcher::new(b"Enter a value:");

        assert!(!matcher.observe(b"plan output\nEnter a "));
        assert!(matcher.observe(b"value:"));
    }

    #[test]
    fn osc777_notification_sanitizes_fields() {
        assert_eq!(
            osc777_notification("account;name\x1b", "apply\nnow"),
            "\x1b]777;notify;account:name ;apply now\x07"
        );
    }

    #[test]
    fn capture_notifies_once_after_triggering_output() {
        let notification = OutputNotification {
            trigger: "Enter a value:".to_string(),
            title: "production".to_string(),
            message: "terraform apply needs confirmation".to_string(),
        };
        let notified = AtomicBool::new(false);
        let mut forwarded = Vec::new();

        let captured = capture_and_forward(
            "Enter a value: Enter a value:".as_bytes(),
            &mut forwarded,
            Some(&notification),
            &notified,
        );

        assert_eq!(captured, "Enter a value: Enter a value:");
        assert_eq!(
            String::from_utf8(forwarded).unwrap(),
            "Enter a value: Enter a value:\x1b]777;notify;production;terraform apply needs confirmation\x07"
        );
    }
}
