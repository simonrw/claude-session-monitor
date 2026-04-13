use clap::Parser;
use server::store;

#[derive(Parser, Debug)]
#[command(
    name = "claude-session-monitor-server",
    about = "Claude session monitor server"
)]
pub struct Args {
    /// Database file path
    #[arg(long, env = "CLAUDE_MONITOR_DB")]
    db: Option<String>,

    /// Host address to bind to
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    /// Port to listen on
    #[arg(long, default_value_t = 7685)]
    port: u16,
}

impl Args {
    fn db_path(&self) -> String {
        if let Some(ref path) = self.db {
            return path.clone();
        }
        let data_dir = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{data_dir}/claude-session-monitor.db")
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let conn = {
        let path = args.db_path();
        store::open_db(&path).expect("failed to open database")
    };

    let app = server::build_app(conn);

    let addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    axum::serve(listener, app).await.expect("server error");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_args() {
        let args = Args::parse_from([
            "server",
            "--db",
            "/tmp/test.db",
            "--port",
            "8080",
            "--host",
            "127.0.0.1",
        ]);
        assert_eq!(args.db, Some("/tmp/test.db".into()));
        assert_eq!(args.port, 8080);
        assert_eq!(args.host, "127.0.0.1");
    }

    #[test]
    fn defaults_when_no_args() {
        let args = Args::parse_from(["server"]);
        assert_eq!(args.db, None);
        assert_eq!(args.port, 7685);
        assert_eq!(args.host, "0.0.0.0");
    }

    #[test]
    fn db_path_uses_explicit_value() {
        let args = Args::parse_from(["server", "--db", "/custom/path.db"]);
        assert_eq!(args.db_path(), "/custom/path.db");
    }

    #[test]
    fn db_path_falls_back_to_home() {
        let args = Args::parse_from(["server"]);
        let path = args.db_path();
        // Should end with the expected filename
        assert!(path.ends_with("/claude-session-monitor.db"));
    }
}
