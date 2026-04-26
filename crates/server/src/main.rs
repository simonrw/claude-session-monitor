use std::path::PathBuf;

use clap::Parser;
use server::store;
use tracing_subscriber::EnvFilter;

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

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Directory containing static web assets
    #[arg(long, env = "CSM_STATIC_DIR")]
    static_dir: Option<PathBuf>,
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
    // Install sentry's panic hook before tracing_subscriber so the chain is:
    // sentry hook -> previous (default) hook. tracing's init won't clobber it.
    let _sentry = common::sentry::init("server");

    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&args.log_level)),
        )
        .init();

    let db_path = args.db_path();
    tracing::info!(db_path, "opening database");
    let conn = store::open_db(&db_path).expect("failed to open database");

    let app = server::build_app(conn, args.static_dir);

    let addr = format!("{}:{}", args.host, args.port);
    tracing::info!(addr, "starting server");
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
            "csm-server",
            "--db",
            "/tmp/test.db",
            "--port",
            "8080",
            "--host",
            "127.0.0.1",
            "--log-level",
            "debug",
        ]);
        assert_eq!(args.db, Some("/tmp/test.db".into()));
        assert_eq!(args.port, 8080);
        assert_eq!(args.host, "127.0.0.1");
        assert_eq!(args.log_level, "debug");
    }

    #[test]
    fn defaults_when_no_args() {
        let args = Args::parse_from(["csm-server"]);
        assert_eq!(args.db, None);
        assert_eq!(args.port, 7685);
        assert_eq!(args.host, "0.0.0.0");
        assert_eq!(args.log_level, "info");
        assert_eq!(args.static_dir, None);
    }

    #[test]
    fn parse_static_dir_arg() {
        let args = Args::parse_from(["csm-server", "--static-dir", "/tmp/web"]);
        assert_eq!(args.static_dir, Some(std::path::PathBuf::from("/tmp/web")));
    }

    #[test]
    fn db_path_uses_explicit_value() {
        let args = Args::parse_from(["csm-server", "--db", "/custom/path.db"]);
        assert_eq!(args.db_path(), "/custom/path.db");
    }

    #[test]
    fn db_path_falls_back_to_home() {
        let args = Args::parse_from(["csm-server"]);
        let path = args.db_path();
        // Should end with the expected filename
        assert!(path.ends_with("/claude-session-monitor.db"));
    }
}
