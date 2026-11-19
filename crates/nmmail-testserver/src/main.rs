//! `nmmail-testserver`: in-memory IMAP + SMTP server for local development of NM Mail.

use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use nmmail_testserver::{Config, TestServer, seed};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
Usage: nmmail-testserver [OPTIONS]

In-memory IMAP + SMTP server (plain TCP, no TLS) for NM Mail development.

Options:
  --imap <ADDR>          IMAP listen address [default: 127.0.0.1:1143]
  --smtp <ADDR>          SMTP listen address [default: 127.0.0.1:1025]
  --user <EMAIL:PASS>    Account, repeatable [default: test@nmmail.local:test]
  --token <TOKEN>        Bearer token accepted by XOAUTH2 for every account [default: test-token]
  --gmail                Emulate Gmail ([Gmail]/... mailboxes, X-GM-EXT-1)
  --no-seed              Do not fill INBOX and Sent with demo messages
  --auto-deliver <SECS>  Deliver a new message to every INBOX every SECS seconds
  -h, --help             Show this help

Log level: RUST_LOG (default: info).";

struct Options {
    config: Config,
    seed: bool,
    auto_deliver: Option<Duration>,
}

/// Parses the command line; `Ok(None)` means `--help`.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut config = Config {
        imap_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 1143)),
        smtp_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 1025)),
        ..Config::default()
    };
    let mut users = Vec::new();
    let mut seed = true;
    let mut auto_deliver = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (name, inline_value) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_string(), Some(value.to_string()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = || {
            inline_value
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match name.as_str() {
            "-h" | "--help" => return Ok(None),
            "--imap" => {
                let v = value()?;
                config.imap_addr = v
                    .parse()
                    .map_err(|_| format!("invalid IMAP address: {v}"))?;
            }
            "--smtp" => {
                let v = value()?;
                config.smtp_addr = v
                    .parse()
                    .map_err(|_| format!("invalid SMTP address: {v}"))?;
            }
            "--user" => {
                let v = value()?;
                match v.split_once(':') {
                    Some((email, password)) if !email.trim().is_empty() => {
                        users.push((email.trim().to_string(), password.to_string()));
                    }
                    _ => return Err(format!("--user expects EMAIL:PASSWORD, got {v}")),
                }
            }
            "--token" => config.xoauth2_token = Some(value()?),
            "--gmail" => config.gmail_mode = true,
            "--no-seed" => seed = false,
            "--auto-deliver" => {
                let v = value()?;
                let secs: u64 = v.parse().ok().filter(|s| *s > 0).ok_or_else(|| {
                    format!("--auto-deliver expects a positive number of seconds, got {v}")
                })?;
                auto_deliver = Some(Duration::from_secs(secs));
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if !users.is_empty() {
        config.users = users;
    }
    Ok(Some(Options {
        config,
        seed,
        auto_deliver,
    }))
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let options = match parse_args(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!("error: {err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let server = match TestServer::start(options.config.clone()).await {
        Ok(server) => server,
        Err(err) => {
            eprintln!("error: {err:#}");
            return ExitCode::FAILURE;
        }
    };

    println!("NM Mail test server (in memory, plain TCP, no TLS)");
    println!("  IMAP   {}", server.imap_addr());
    println!("  SMTP   {}", server.smtp_addr());
    println!(
        "  Mode   {}",
        if options.config.gmail_mode {
            "Gmail emulation ([Gmail]/... mailboxes, X-GM-EXT-1)"
        } else {
            "generic IMAP (Sent, Drafts, Trash, Junk, Archive)"
        }
    );
    println!("  Accounts (email / password):");
    for (email, password) in &options.config.users {
        let seeded = if options.seed {
            format!("  [{} demo messages]", server.seed_demo_messages(email))
        } else {
            String::new()
        };
        println!("    {email} / {password}{seeded}");
    }
    match &options.config.xoauth2_token {
        Some(token) => println!("  XOAUTH2 bearer token (any account): {token}"),
        None => println!("  XOAUTH2 disabled"),
    }
    if let Some(period) = options.auto_deliver {
        println!(
            "  Auto-delivery: a new message to every INBOX every {} s",
            period.as_secs()
        );
    }
    println!("Press Ctrl+C to stop.");

    let mut ticker = options
        .auto_deliver
        .map(|period| tokio::time::interval_at(tokio::time::Instant::now() + period, period));
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut counter = 0u64;
    loop {
        let tick = async {
            match ticker.as_mut() {
                Some(ticker) => {
                    ticker.tick().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            res = &mut ctrl_c => {
                if let Err(err) = res {
                    eprintln!("error: cannot listen for Ctrl+C: {err}");
                }
                break;
            }
            () = tick => {
                counter += 1;
                for (email, _) in &options.config.users {
                    let uid = server.deliver(email, "INBOX", &seed::demo_message(counter, email));
                    println!("Delivered message #{counter} to {email} (INBOX UID {uid})");
                }
            }
        }
    }
    println!("Shutting down...");
    server.shutdown().await;
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_command_line() {
        let options = parse(&[
            "--imap",
            "0.0.0.0:2143",
            "--smtp=127.0.0.1:2025",
            "--user",
            "a@b.c:p:w",
            "--user",
            "x@y.z:q",
            "--gmail",
            "--no-seed",
            "--auto-deliver",
            "30",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(options.config.imap_addr.port(), 2143);
        assert_eq!(options.config.smtp_addr.port(), 2025);
        assert_eq!(
            options.config.users[0],
            ("a@b.c".to_string(), "p:w".to_string())
        );
        assert_eq!(options.config.users.len(), 2);
        assert!(options.config.gmail_mode && !options.seed);
        assert_eq!(options.auto_deliver, Some(Duration::from_secs(30)));

        let defaults = parse(&[]).unwrap().unwrap();
        assert_eq!(defaults.config.imap_addr.port(), 1143);
        assert_eq!(defaults.config.users[0].0, "test@nmmail.local");
        assert!(defaults.seed);

        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&["--imap"]).is_err());
        assert!(parse(&["--user", "nocolon"]).is_err());
        assert!(parse(&["--auto-deliver", "0"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
