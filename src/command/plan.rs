use crate::{
    args::PlanArgs,
    clipboard,
    command::current_branch,
    config::Config,
    grouped_dirs::{GroupedDirs, LoginPolicy},
    pretty_format,
};

pub fn plan(args: &PlanArgs, config: &Config) {
    let affected_modules = current_branch::affected_modules("plan");
    if affected_modules.is_empty() {
        return;
    }
    println!("\nRunning plan for each module. Nothing will be applied.");

    let output = GroupedDirs::new(&affected_modules).plan_all(config, LoginPolicy::Reuse);
    let output_str = pretty_format::format_output(output);
    println!("{output_str}");
    if args.clipboard {
        clipboard::copy_to_clipboard(&output_str);
    }
}
