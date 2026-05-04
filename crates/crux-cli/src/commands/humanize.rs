use std::io::{Read, Write};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;

use crux_humanizer::{Humanizer, Mode};

use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, default_value = "concise")]
    pub mode: String,

    #[arg(long, short = 'i', conflicts_with = "file")]
    pub input: Option<String>,

    #[arg(long, short = 'f', value_name = "PATH")]
    pub file: Option<PathBuf>,

    #[arg(long)]
    pub stats: bool,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    let mode = Mode::from_str(&args.mode).map_err(anyhow::Error::msg)?;
    let input = read_input(args)?;

    let result = Humanizer::new(mode).rewrite(&input);

    if cli.json {
        let payload = serde_json::json!({
            "mode": result.mode.as_str(),
            "text": result.text,
            "stats": {
                "original_chars": result.stats.original_chars,
                "rewritten_chars": result.stats.rewritten_chars,
                "chars_saved": result.stats.chars_saved(),
                "original_words": result.stats.original_words,
                "rewritten_words": result.stats.rewritten_words,
                "words_saved": result.stats.words_saved(),
                "edits_applied": result.stats.edits_applied,
            },
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(result.text.as_bytes())?;
    if !result.text.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    drop(stdout);

    if args.stats {
        eprintln!(
            "humanize[{}]: chars {}→{} (-{}), words {}→{} (-{}), edits {}",
            result.mode,
            result.stats.original_chars,
            result.stats.rewritten_chars,
            result.stats.chars_saved(),
            result.stats.original_words,
            result.stats.rewritten_words,
            result.stats.words_saved(),
            result.stats.edits_applied,
        );
    }

    Ok(())
}

fn read_input(args: &Args) -> Result<String> {
    if let Some(s) = &args.input {
        if s == "-" {
            return read_stdin();
        }
        return Ok(s.clone());
    }
    if let Some(p) = &args.file {
        if p.as_os_str() == "-" {
            return read_stdin();
        }
        return std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()));
    }
    read_stdin()
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("read stdin")?;
    if buf.is_empty() {
        return Err(anyhow::anyhow!(
            "no input: pass --input, --file, or pipe text on stdin"
        ));
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modes_via_args() {
        for m in Mode::ALL {
            let parsed = Mode::from_str(m.as_str()).expect("known mode");
            assert_eq!(parsed, *m);
        }
    }

    #[test]
    fn inline_input_returned_literally() {
        let args = Args {
            mode: "concise".into(),
            input: Some("literal text".into()),
            file: None,
            stats: false,
        };
        let got = read_input(&args).unwrap();
        assert_eq!(got, "literal text");
    }

    #[test]
    fn file_input_reads_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("in.txt");
        std::fs::write(&p, "from file").unwrap();
        let args = Args {
            mode: "concise".into(),
            input: None,
            file: Some(p),
            stats: false,
        };
        let got = read_input(&args).unwrap();
        assert_eq!(got, "from file");
    }
}
