use camino::Utf8PathBuf;

use crate::cmd_runner::PlanOutcome;

/// Print two lists of directories, one for each outcome
pub fn format_output(output: Vec<(Utf8PathBuf, PlanOutcome)>) -> String {
    let mut output_str = String::from("## 📃📃 Plan summary 📃📃\n");
    let (no_changes, changes): (Vec<_>, Vec<_>) = output
        .into_iter()
        .partition(|(_, o)| matches!(o, PlanOutcome::NoChanges));
    if !no_changes.is_empty() {
        output_str.push_str("\nNo changes detected (apply not needed):\n");
    }
    for (dir, _) in no_changes {
        output_str.push_str(&format!("✅ {dir}\n"));
    }

    if !changes.is_empty() {
        output_str.push_str("\nChanges detected (apply needed):\n");
    }
    for (dir, _) in &changes {
        output_str.push_str(&format!("❌ {dir}\n"));
    }

    if !changes.is_empty() {
        output_str.push_str("\n## 📃📃 Plan output 📃📃\n");
    }
    for (dir, output) in &changes {
        output_str.push_str(&format!("👉 {dir}:\n"));
        if let PlanOutcome::Changes(output) = output {
            output_str.push_str(&format!("\n```\n{output}\n```\n"));
        } else {
            panic!("Expected changes, got no changes");
        }
    }

    output_str
}
