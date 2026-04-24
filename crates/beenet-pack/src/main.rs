use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use beenet_common::BeenetCid;
use beenet_manifest::{extract, embed, Manifest};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "beenet-pack", about = "Pack and inspect Beenet tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Embed `beenet.toml` into a built wasm file.
    Build {
        #[arg(long)]
        wasm: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Print CID + embedded manifest from a packaged wasm file.
    Inspect {
        wasm: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            wasm,
            manifest,
            out,
        } => build(wasm, manifest, out),
        Command::Inspect { wasm } => inspect(wasm),
    }
}

fn build(wasm_path: PathBuf, manifest_path: PathBuf, out_path: PathBuf) -> Result<()> {
    let wasm = fs::read(&wasm_path).with_context(|| format!("read wasm `{}`", wasm_path.display()))?;
    let manifest_toml = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read manifest `{}`", manifest_path.display()))?;
    let manifest = Manifest::from_toml(&manifest_toml)?;
    let packaged = embed(&wasm, &manifest)?;
    let cid = BeenetCid::from_bytes(&packaged);

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir `{}`", parent.display()))?;
    }
    fs::write(&out_path, &packaged).with_context(|| format!("write `{}`", out_path.display()))?;

    println!("CID: {}", cid);
    println!("OUT: {}", out_path.display());
    println!("SIZE: {}", packaged.len());
    Ok(())
}

fn inspect(wasm_path: PathBuf) -> Result<()> {
    let wasm = fs::read(&wasm_path).with_context(|| format!("read `{}`", wasm_path.display()))?;
    let cid = BeenetCid::from_bytes(&wasm);
    let manifest = extract(&wasm)?;
    println!("CID: {}", cid);
    println!("SIZE: {}", wasm.len());
    println!("INTERFACE: {}", manifest.task.interface);
    println!("MANIFEST:");
    println!("{}", manifest.to_toml()?);
    Ok(())
}
