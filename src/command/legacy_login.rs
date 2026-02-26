use secrecy::ExposeSecret;

use crate::{aws, config::Config};

pub fn login_to_legacy_aws_account(config: &Config) {
    let env_vars = aws::legacy_login(config.op_legacy_item_id.as_deref());
    for (key, value) in env_vars {
        println!("export {key}={}", shell_quote(value.expose_secret()));
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("abc'def"), "'abc'\\''def'");
    }
}
