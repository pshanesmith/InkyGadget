use anyhow::{bail, Context, Result};
use inkyframe::{cli, config::Config};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp(None).init();
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg_path = flag(&args, "--config").unwrap_or_else(|| "/etc/inkyframe/config.toml".into());
    match args.first().map(String::as_str) {
        Some("preview") => cli::preview(&args[1..]),
        Some("run") => {
            let cfg = Config::load(cfg_path.as_ref())?;
            cli::run(cfg, &args[1..])
        }
        Some("geocode") => cli::geocode(&args[1..], &cfg_path),
        Some("check-config") => cli::check_config(cfg_path.as_ref()),
        // A config that exists must be valid (a missing one just means the defaults).
        Some("ha-discover") => {
            let cfg = if std::path::Path::new(&cfg_path).exists() { Config::load(cfg_path.as_ref())? } else { Config::default() };
            cli::ha_discover(&cfg)
        }
        Some("hwtest") => cli::hwtest(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("inkyframe {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("-h") | Some("--help") | None => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Some(other) => bail!("unknown command {other:?} (try --help)"),
    }
    .context("failed")
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}
