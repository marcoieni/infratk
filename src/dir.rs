use camino::{Utf8Path, Utf8PathBuf};

pub fn current_dir() -> Utf8PathBuf {
    let current_dir = std::env::current_dir().unwrap();
    Utf8PathBuf::from_path_buf(current_dir).unwrap()
}

pub fn strip_current_dir(path: &Utf8Path) -> Utf8PathBuf {
    let curr_dir = current_dir();
    if let Ok(stripped_path) = path.strip_prefix(&curr_dir) {
        stripped_path.to_path_buf()
    } else {
        path.to_path_buf()
    }
}

pub fn current_dir_is_simpleinfra() -> bool {
    let output = std::process::Command::new("git")
        .args(["remote", "-v"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.lines().any(|line| {
                line.contains("github.com:rust-lang/simpleinfra")
                    || line.contains("github.com/rust-lang/simpleinfra")
            })
        }
        _ => false,
    }
}

pub fn get_stripped_parent(path: &Utf8PathBuf) -> Utf8PathBuf {
    let parent = path.parent().unwrap();
    strip_current_dir(parent)
}
