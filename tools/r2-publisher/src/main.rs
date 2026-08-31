use aws_config::BehaviorVersion;
use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Options {
    appcast: PathBuf,
    zip: PathBuf,
    dmg: PathBuf,
    version: String,
    build_version: String,
    dry_run: bool,
}

fn usage() -> &'static str {
    "usage: oh-my-tab-r2-publisher [--appcast PATH] [--zip PATH] [--dmg PATH] \
     [--version VERSION] [--build-version BUILD] [--dry-run]"
}

fn env_or(name: &str, fallback: &str) -> String {
    env::var(name).unwrap_or_else(|_| fallback.to_string())
}

fn parse_options() -> Result<Options, String> {
    let mut appcast = PathBuf::from(env_or("R2_APPCAST_PATH", "dist/appcast.xml"));
    let mut zip = PathBuf::from(env_or("R2_ZIP_PATH", "dist/Oh-My-Tab.zip"));
    let mut dmg = PathBuf::from(env_or("R2_DMG_PATH", "dist/Oh-My-Tab.dmg"));
    let mut version = env::var("R2_VERSION").unwrap_or_default();
    let mut build_version = env::var("R2_BUILD_VERSION").unwrap_or_default();
    let mut dry_run = false;

    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--dry-run" => dry_run = true,
            "--appcast" | "--zip" | "--dmg" | "--version" | "--build-version" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| format!("missing value for {arg}\n{}", usage()))?;
                match arg {
                    "--appcast" => appcast = PathBuf::from(value),
                    "--zip" => zip = PathBuf::from(value),
                    "--dmg" => dmg = PathBuf::from(value),
                    "--version" => version = value.clone(),
                    "--build-version" => build_version = value.clone(),
                    _ => unreachable!(),
                }
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}\n{}", usage())),
        }
        i += 1;
    }

    if version.trim().is_empty() {
        return Err("missing version (pass --version or set R2_VERSION)".to_string());
    }
    if build_version.trim().is_empty() {
        return Err(
            "missing build version (pass --build-version or set R2_BUILD_VERSION)".to_string(),
        );
    }

    Ok(Options {
        appcast,
        zip,
        dmg,
        version,
        build_version,
        dry_run,
    })
}

fn require_file(path: &Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("required file not found: {}", path.display()))
    }
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("missing environment variable {name}"))
}

fn object_key(
    prefix: &str,
    artifact_basename: &str,
    version: &str,
    build_version: &str,
    extension: &str,
) -> String {
    let prefix = prefix.trim_matches('/');
    let basename = artifact_basename.trim();
    let basename = if basename.is_empty() {
        "Oh-My-Tab"
    } else {
        basename
    };
    let filename = format!("{basename}-{version}-{build_version}.{extension}");
    if prefix.is_empty() {
        filename
    } else {
        format!("{prefix}/{filename}")
    }
}

fn public_url(base: &str, key: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), key)
}

async fn upload_file(
    client: &Client,
    bucket: &str,
    key: &str,
    path: &Path,
    content_type: &str,
    cache_control: &str,
) -> Result<(), String> {
    let body = ByteStream::from_path(path)
        .await
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(body)
        .content_type(content_type)
        .cache_control(cache_control)
        .send()
        .await
        .map_err(|e| format!("upload {key}: {e}"))?;
    Ok(())
}

async fn run() -> Result<(), String> {
    let options = parse_options()?;
    require_file(&options.appcast)?;
    require_file(&options.zip)?;
    require_file(&options.dmg)?;

    let prefix = env_or("R2_RELEASE_PREFIX", "releases");
    let appcast_key = env_or("R2_APPCAST_KEY", "appcast.xml");
    let artifact_basename = env_or("R2_ARTIFACT_BASENAME", "Oh-My-Tab");
    let zip_key = object_key(
        &prefix,
        &artifact_basename,
        &options.version,
        &options.build_version,
        "zip",
    );
    let dmg_key = object_key(
        &prefix,
        &artifact_basename,
        &options.version,
        &options.build_version,
        "dmg",
    );
    // This base is used only for the URLs printed into the release plan. The actual PUT requests
    // below always use R2_ENDPOINT (or the endpoint derived from R2_ACCOUNT_ID) and R2_BUCKET.
    let public_base = env_or("R2_PUBLIC_BASE_URL", "https://download.oh-my-tab.app");

    println!("R2 release plan:");
    println!(
        "  appcast: {} -> {}",
        options.appcast.display(),
        public_url(&public_base, &appcast_key)
    );
    println!(
        "  zip:     {} -> {}",
        options.zip.display(),
        public_url(&public_base, &zip_key)
    );
    println!(
        "  dmg:     {} -> {}",
        options.dmg.display(),
        public_url(&public_base, &dmg_key)
    );

    if options.dry_run {
        println!("dry run: no files uploaded");
        return Ok(());
    }

    let access_key = required_env("R2_ACCESS_KEY_ID")?;
    let secret_key = required_env("R2_SECRET_ACCESS_KEY")?;
    let bucket = required_env("R2_BUCKET")?;
    let endpoint = match env::var("R2_ENDPOINT") {
        Ok(value) => value,
        Err(_) => {
            let account_id = required_env("R2_ACCOUNT_ID")?;
            format!("https://{account_id}.r2.cloudflarestorage.com")
        }
    };
    let region = env_or("R2_REGION", "auto");
    let credentials = Credentials::new(access_key, secret_key, None, None, "cloudflare-r2");
    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region))
        .endpoint_url(&endpoint)
        .credentials_provider(credentials)
        .load()
        .await;
    let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
        .endpoint_url(&endpoint)
        .force_path_style(true)
        .build();
    let client = Client::from_conf(s3_config);

    // Upload immutable archives first; publish appcast last so clients never see a feed that
    // points at an object that has not arrived yet.
    upload_file(
        &client,
        &bucket,
        &zip_key,
        &options.zip,
        "application/zip",
        "public, max-age=31536000, immutable",
    )
    .await?;
    upload_file(
        &client,
        &bucket,
        &dmg_key,
        &options.dmg,
        "application/octet-stream",
        "public, max-age=31536000, immutable",
    )
    .await?;
    upload_file(
        &client,
        &bucket,
        &appcast_key,
        &options.appcast,
        "application/xml",
        "public, max-age=60",
    )
    .await?;

    println!("R2 upload complete");
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
