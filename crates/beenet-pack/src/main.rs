use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use aws_smithy_http_client::{proxy::ProxyConfig, tls, Builder as HttpClientBuilder, Connector};
use beenet_common::config::{
    load_file, resolve_config_path_with_cli, resolve_oss_settings, OssCliOverrides, OssSettings,
};
use beenet_common::BeenetCid;
use beenet_manifest::{embed, extract, Manifest};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "beenet-pack", about = "Pack and inspect Beenet tasks")]
struct Cli {
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,

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
    Inspect { wasm: PathBuf },
    /// Upload packaged wasm (with embedded manifest) to S3-compatible storage (e.g. Aliyun OSS).
    Upload {
        #[arg(long)]
        wasm: PathBuf,
        #[arg(long)]
        oss_endpoint: Option<String>,
        #[arg(long)]
        oss_bucket: Option<String>,
        #[arg(long)]
        oss_access_key_id: Option<String>,
        #[arg(long)]
        oss_access_key_secret: Option<String>,
        #[arg(long)]
        oss_region: Option<String>,
        #[arg(long)]
        key_prefix: Option<String>,
        #[arg(long)]
        oss_force_path_style: Option<bool>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            wasm,
            manifest,
            out,
        } => build(wasm, manifest, out),
        Command::Inspect { wasm } => inspect(wasm),
        Command::Upload {
            wasm,
            oss_endpoint,
            oss_bucket,
            oss_access_key_id,
            oss_access_key_secret,
            oss_region,
            key_prefix,
            oss_force_path_style,
        } => {
            let path = resolve_config_path_with_cli(cli.config.clone(), &argv);
            if !path.exists() {
                anyhow::bail!(
                    "missing config file `{}` for `upload` (add [oss] or pass --config)",
                    path.display()
                );
            }
            let file_cfg = load_file(&path)?;
            let oss_cli = OssCliOverrides {
                endpoint: oss_endpoint,
                bucket: oss_bucket,
                access_key_id: oss_access_key_id,
                access_key_secret: oss_access_key_secret,
                region: oss_region,
                key_prefix,
                force_path_style: oss_force_path_style,
            };
            let oss = resolve_oss_settings(&file_cfg, &oss_cli)?;
            upload(wasm, oss).await
        }
    }
}

fn build(wasm_path: PathBuf, manifest_path: PathBuf, out_path: PathBuf) -> Result<()> {
    let wasm =
        fs::read(&wasm_path).with_context(|| format!("read wasm `{}`", wasm_path.display()))?;
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

fn normalize_prefix(prefix: &str) -> String {
    if prefix.is_empty() {
        String::new()
    } else if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

async fn upload(wasm_path: PathBuf, oss: OssSettings) -> Result<()> {
    let bytes = fs::read(&wasm_path).with_context(|| format!("read `{}`", wasm_path.display()))?;
    let cid = BeenetCid::from_bytes(&bytes);
    let _manifest = extract(&bytes)
        .context("packaged wasm must contain beenet manifest (run `beenet-pack build` first)")?;

    let prefix = normalize_prefix(&oss.key_prefix);
    let key = format!("{prefix}{cid}");

    let credentials = Credentials::new(
        oss.access_key_id,
        oss.access_key_secret,
        None,
        None,
        "beenet-pack",
    );
    let shared = SharedCredentialsProvider::new(credentials);
    let http_client = HttpClientBuilder::new().build_with_connector_fn(|settings, _runtime| {
        let mut connector = Connector::builder().proxy_config(ProxyConfig::from_env());
        if let Some(settings) = settings.cloned() {
            connector = connector.connector_settings(settings);
        }
        connector
            .tls_provider(tls::Provider::rustls(
                tls::rustls_provider::CryptoMode::Ring,
            ))
            .build()
    });
    let conf = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(oss.region))
        .endpoint_url(oss.endpoint.trim_end_matches('/'))
        .credentials_provider(shared)
        .http_client(http_client)
        .force_path_style(oss.force_path_style)
        .build();
    let client = Client::from_conf(conf);

    client
        .put_object()
        .bucket(&oss.bucket)
        .key(&key)
        .content_type("application/wasm")
        .body(ByteStream::from(bytes))
        .send()
        .await
        .with_context(|| format!("S3 PutObject bucket={} key={key}", oss.bucket))?;

    println!("CID: {cid}");
    println!("OSS_KEY: {key}");
    println!("BUCKET: {}", oss.bucket);
    let base_tail = prefix.trim_end_matches('/');
    if base_tail.is_empty() {
        println!(
            "Worker [worker].wasm_fetch_base: https://{}.{{region}}.aliyuncs.com (no trailing slash; GET {{base}}/{{cid}})",
            oss.bucket
        );
    } else {
        println!(
            "Worker [worker].wasm_fetch_base: https://{}.{{region}}.aliyuncs.com/{base_tail}",
            oss.bucket
        );
    }
    Ok(())
}
