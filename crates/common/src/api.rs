const DEFAULT_SERVER_URL: &str = "http://localhost:7685";

pub fn resolve_server_url(cli_arg: Option<&str>) -> String {
    if let Some(url) = cli_arg {
        return url.to_string();
    }
    if let Ok(url) = std::env::var("CLAUDE_MONITOR_URL") {
        return url;
    }
    DEFAULT_SERVER_URL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_arg_takes_precedence_over_env_and_default() {
        unsafe { std::env::set_var("CLAUDE_MONITOR_URL", "http://env:7685") };
        let url = resolve_server_url(Some("http://cli:7685"));
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        assert_eq!(url, "http://cli:7685");
    }

    #[test]
    fn env_var_takes_precedence_over_default() {
        unsafe { std::env::set_var("CLAUDE_MONITOR_URL", "http://env:7685") };
        let url = resolve_server_url(None);
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        assert_eq!(url, "http://env:7685");
    }

    #[test]
    fn default_returned_when_no_cli_arg_or_env_var() {
        unsafe { std::env::remove_var("CLAUDE_MONITOR_URL") };
        let url = resolve_server_url(None);
        assert_eq!(url, "http://localhost:7685");
    }
}
