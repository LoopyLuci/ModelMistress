//! `mm-overlay`: publishes this desktop's ModelMistress hub through the Transferd overlay, so a paired phone can reach it
//! from any network. The hub itself stays on loopback; this tool is the only thing that carries it out.
//!
//! ```text
//! mm-overlay id    --home <dir>
//! mm-overlay serve --home <dir> --hub <127.0.0.1:port> [--name <name>] [--listen <127.0.0.1:port>]
//!                  [--direct <host:port>]... [--relay <host:port>]... [--invite-ttl <seconds>]
//! ```
//!
//! `serve` runs until Ctrl-C. `--hub` is the hub's address from its `control.json`. `--invite-ttl` makes a pairing
//! invite for the phone, printed at start. While it runs, `serve` reads commands from stdin: `invite [seconds]` prints
//! another invite, and `status` shows the node. An invite pairs one device, then it is spent; it is only good while this
//! process runs.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use mm_overlay::{Overlay, Settings, ALLOW_PAIRED, HUB_SERVICE};
use tokio::io::{AsyncBufReadExt, BufReader};

/// The lifetime of an invite made with `invite` and no number, when `serve` was started without `--invite-ttl`.
const DEFAULT_INVITE_TTL: u64 = 900;

const USAGE: &str = "usage:
  mm-overlay id    --home <dir>
  mm-overlay serve --home <dir> --hub <127.0.0.1:port> [--name <name>] [--listen <127.0.0.1:port>]
                   [--direct <host:port>]... [--relay <host:port>]... [--invite-ttl <seconds>]";

#[derive(Default)]
struct Args {
    command: String,
    home: Option<PathBuf>,
    hub: Option<SocketAddr>,
    name: Option<String>,
    listen: Option<SocketAddr>,
    direct: Vec<String>,
    relays: Vec<String>,
    invite_ttl: u64,
}

fn parse(mut raw: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args {
        command: raw.next().ok_or_else(|| USAGE.to_string())?,
        ..Args::default()
    };
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--home" => args.home = Some(PathBuf::from(value()?)),
            "--hub" => args.hub = Some(parse_addr(&flag, &value()?)?),
            "--listen" => args.listen = Some(parse_addr(&flag, &value()?)?),
            "--name" => args.name = Some(value()?),
            "--direct" => args.direct.push(value()?),
            "--relay" => args.relays.push(value()?),
            "--invite-ttl" => {
                let text = value()?;
                args.invite_ttl = text
                    .parse()
                    .map_err(|_| format!("--invite-ttl takes whole seconds, not {text:?}"))?;
            }
            other => return Err(format!("unknown option {other:?}\n{USAGE}")),
        }
    }
    Ok(args)
}

fn parse_addr(flag: &str, text: &str) -> Result<SocketAddr, String> {
    text.parse()
        .map_err(|_| format!("{flag} takes an address such as 127.0.0.1:8000, not {text:?}"))
}

fn default_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "desktop".to_string())
}

#[tokio::main]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("mm-overlay: {message}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let args = parse(std::env::args().skip(1))?;
    let home = args
        .home
        .clone()
        .ok_or_else(|| format!("--home is required\n{USAGE}"))?;
    match args.command.as_str() {
        "id" => {
            let overlay = start(&home, &args, None).await?;
            println!("{}", overlay.node_id());
            Ok(())
        }
        "serve" => serve(&home, args).await,
        other => Err(format!("unknown command {other:?}\n{USAGE}")),
    }
}

async fn start(home: &std::path::Path, args: &Args, listen: Option<SocketAddr>) -> Result<Overlay, String> {
    let settings = Settings {
        name: args.name.clone().unwrap_or_else(default_name),
        relays: args.relays.clone(),
        direct: args.direct.clone(),
        listen,
    };
    Overlay::start(home, settings).await
}

async fn serve(home: &std::path::Path, args: Args) -> Result<(), String> {
    let hub = args
        .hub
        .ok_or_else(|| format!("--hub is required: the hub's address from its control.json\n{USAGE}"))?;
    if !is_loopback(hub.ip()) {
        return Err(format!(
            "--hub {hub} is not a loopback address; the hub stays on loopback and the overlay is the only way out"
        ));
    }
    let overlay = start(home, &args, args.listen).await?;
    overlay
        .expose(HUB_SERVICE, hub, &[ALLOW_PAIRED], "ModelMistress hub")
        .map_err(|e| format!("cannot publish the hub: {e}"))?;

    println!("node:    {}", overlay.node_id());
    println!("service: {HUB_SERVICE} -> {hub} (paired nodes only)");
    match overlay.listener() {
        Some(addr) => println!("listen:  {addr} (direct, loopback only unless you set otherwise)"),
        None => println!("listen:  none (outbound only)"),
    }
    let relays = overlay.relays();
    println!(
        "relays:  {}",
        if relays.is_empty() { "none".to_string() } else { relays.join(", ") }
    );
    if args.invite_ttl > 0 {
        print_invite(&overlay, args.invite_ttl);
    }
    println!("running; type `invite [seconds]` for another invite, `status`, or press Ctrl-C to stop");

    // Commands arrive on stdin while it is open. When stdin closes (a service, say), the node keeps serving until Ctrl-C.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut reading = true;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            line = lines.next_line(), if reading => match line {
                Ok(Some(text)) => command(&overlay, text.trim(), &args),
                _ => reading = false,
            },
        }
    }
    println!("stopping");
    Ok(())
}

/// Prints a fresh invite. An invite pairs one node: the desktop forgets it once a node has used it.
fn print_invite(overlay: &Overlay, ttl_s: u64) {
    println!("invite (valid {ttl_s} s, pairs one device): {}", overlay.invite(ttl_s));
}

fn command(overlay: &Overlay, text: &str, args: &Args) {
    let mut words = text.split_whitespace();
    match words.next() {
        None => {}
        Some("invite") => {
            let ttl = match words.next() {
                None if args.invite_ttl > 0 => args.invite_ttl,
                None => DEFAULT_INVITE_TTL,
                Some(seconds) => match seconds.parse::<u64>() {
                    Ok(seconds) if seconds > 0 => seconds,
                    _ => {
                        println!("invite takes a number of seconds, for example `invite 900`");
                        return;
                    }
                },
            };
            print_invite(overlay, ttl);
        }
        Some("status") => {
            println!("node:  {}", overlay.node_id());
            println!("peers: {}", overlay.peers().len());
            println!("relays: {}", overlay.relays().len());
        }
        Some(other) => println!("unknown command {other:?}; try `invite [seconds]` or `status`"),
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> impl Iterator<Item = String> + '_ {
        line.split_whitespace().map(str::to_string)
    }

    #[test]
    fn serve_takes_its_options() {
        let args = parse(words(
            "serve --home D:/h --hub 127.0.0.1:8123 --listen 127.0.0.1:7000 --direct 127.0.0.1:7000 --invite-ttl 900",
        ))
        .unwrap();
        assert_eq!(args.command, "serve");
        assert_eq!(args.hub, Some("127.0.0.1:8123".parse().unwrap()));
        assert_eq!(args.listen, Some("127.0.0.1:7000".parse().unwrap()));
        assert_eq!(args.direct, vec!["127.0.0.1:7000".to_string()]);
        assert_eq!(args.invite_ttl, 900);
    }

    #[test]
    fn a_bad_address_or_an_unknown_option_is_reported() {
        assert!(parse(words("serve --hub not-an-address")).is_err());
        assert!(parse(words("serve --bogus 1")).is_err());
        assert!(parse(words("serve --invite-ttl soon")).is_err());
    }

    #[test]
    fn only_loopback_counts_as_loopback() {
        assert!(is_loopback("127.0.0.1".parse().unwrap()));
        assert!(is_loopback("::1".parse().unwrap()));
        assert!(!is_loopback("192.168.1.20".parse().unwrap()));
        assert!(!is_loopback("0.0.0.0".parse().unwrap()));
    }
}
