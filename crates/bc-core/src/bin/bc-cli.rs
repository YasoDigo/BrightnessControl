//! Hardware test tool.
//!
//!   bc-cli list
//!   bc-cli get <id|index>
//!   bc-cli set <id|index> <percent>
//!   bc-cli volume <id|index> <percent>
//!   bc-cli input <id|index> <code>
//!   bc-cli import [path-to-Settings_v2.xml]

use bc_core::discovery::{self, Discovered};
use bc_core::{legacy, logic, paths, vcp};

fn pick<'a>(list: &'a mut [Discovered], key: &str) -> anyhow::Result<&'a mut Discovered> {
    if let Ok(i) = key.parse::<usize>() {
        if i < list.len() {
            return Ok(&mut list[i]);
        }
    }
    list.iter_mut()
        .find(|d| d.id.eq_ignore_ascii_case(key))
        .ok_or_else(|| anyhow::anyhow!("no monitor '{key}' (try `bc-cli list`)"))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("list");

    if cmd == "import" {
        let path = args.get(1).map(Into::into).unwrap_or_else(paths::legacy_settings_file);
        let xml = std::fs::read_to_string(&path)?;
        let s = legacy::import(&xml)?;
        println!("{}", toml::to_string_pretty(&s)?);
        return Ok(());
    }

    let t = std::time::Instant::now();
    let mut list = discovery::discover();
    eprintln!("scan took {} ms", t.elapsed().as_millis());

    match cmd {
        "list" => {
            for (i, d) in list.iter().enumerate() {
                let b = d.caps.brightness.map(|(c, m)| format!("{}% (raw {c}/{m})", logic::raw_to_pct(c, m)));
                let v = d.caps.volume.map(|(c, m)| format!("{}%", logic::raw_to_pct(c, m)));
                let inp = d.caps.input.map(|c| {
                    let c = vcp::normalize_input(c);
                    vcp::input(c).map(|i| i.name.to_string()).unwrap_or(format!("code {c}"))
                });
                println!(
                    "[{i}] {:<28} {:<22} {:?}\n     brightness={} volume={} input={}",
                    d.id,
                    d.name,
                    d.kind,
                    b.unwrap_or("-".into()),
                    v.unwrap_or("-".into()),
                    inp.unwrap_or("-".into())
                );
            }
        }
        "get" => {
            let d = pick(&mut list, args.get(1).map(String::as_str).unwrap_or("0"))?;
            for code in [vcp::BRIGHTNESS, vcp::VOLUME, vcp::INPUT_SOURCE] {
                println!("{code:#04x}: {:?}", d.backend.read(code));
            }
        }
        "set" | "volume" | "input" => {
            let d = pick(&mut list, args.get(1).map(String::as_str).unwrap_or("0"))?;
            let value: u32 = args.get(2).ok_or_else(|| anyhow::anyhow!("value missing"))?.parse()?;
            let (code, raw) = match cmd {
                "set" => (vcp::BRIGHTNESS, logic::pct_to_raw(value as u8, d.caps.brightness.map(|b| b.1).unwrap_or(100))),
                "volume" => (vcp::VOLUME, logic::pct_to_raw(value as u8, d.caps.volume.map(|b| b.1).unwrap_or(100))),
                _ => (vcp::INPUT_SOURCE, value),
            };
            let t = std::time::Instant::now();
            d.backend.write(code, raw)?;
            println!("{} {code:#04x} <- {raw} ({} ms)", d.id, t.elapsed().as_millis());
            std::thread::sleep(std::time::Duration::from_millis(100));
            println!("read back: {:?}", d.backend.read(code));
        }
        other => anyhow::bail!("unknown command '{other}'"),
    }
    Ok(())
}
