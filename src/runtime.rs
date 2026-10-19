//! Installs what the chat runs on, next to the plugin's data: a pinned Node.js
//! release (verified by SHA-256), and the Ghostly CLI and web app built from
//! the original repository's latest release tag, unmodified. The web app is
//! built for the POM proxy path it is served under ([`WEB_BASE`]).
//!
//! Every start of the plugin asks GitHub for the latest Ghostly release. A tag
//! not built yet is downloaded as source, built with `npm` and installed into
//! its own prefix; the previous build stays until the new one is complete.
//! Offline, the newest complete build is used.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Node.js release the CLI runs on (Ghostly asks for 22.12 or newer).
pub const NODE_VERSION: &str = "v24.22.0";
/// The original repository; nothing in it is patched.
pub const GHOSTLY_REPOSITORY: &str = "MiguelMedeiros/ghostly";
/// The CLI's workspace inside the repository.
const CLI_WORKSPACE: &str = "@ghostlytools/cli";
/// The web app's workspace inside the repository.
const WEB_WORKSPACE: &str = "@ghostly/web";
/// Where the POM serves the web app: the plugin's proxy, under `/web/`.
pub const WEB_BASE: &str = "/api/ui/plugins/ghostly/proxy/web/";

/// `(platform, archive name, SHA-256)` from nodejs.org's `SHASUMS256.txt`.
const NODE_ARCHIVES: &[(&str, &str, &str)] = &[
    (
        "linux-x86_64",
        "node-v24.22.0-linux-x64.tar.gz",
        "28ea0a438de9a13bd7891fd758eee6c7819ee8af5442dfd8d45bd65e56d4228c",
    ),
    (
        "linux-aarch64",
        "node-v24.22.0-linux-arm64.tar.gz",
        "b7e7991d5407815adfa74a2a9cd4db683552c8ce32ff4a295c22e972849c3f5d",
    ),
    (
        "macos-aarch64",
        "node-v24.22.0-darwin-arm64.tar.gz",
        "6a9724d9b50a5f22b4816cded441b5a8d9ddae546191a5e5b7efa5a5e2ee26d5",
    ),
];

/// What the rest of the plugin needs to run the CLI.
#[derive(Debug, Clone)]
pub struct Runtime {
    pub node: PathBuf,
    pub cli: PathBuf,
    /// The built web app (`index.html` and its assets).
    pub web: PathBuf,
    pub version: String,
}

impl Runtime {
    /// A command running the Ghostly CLI with `home` as its `GHOSTLY_HOME`.
    pub fn command(&self, home: &Path) -> Command {
        let mut command = Command::new(&self.node);
        command
            .arg(&self.cli)
            .env("GHOSTLY_HOME", home)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        if let Some(bin) = self.node.parent() {
            command.env("PATH", search_path(bin));
        }
        command
    }
}

fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("macos", "aarch64") => Some("macos-aarch64"),
        _ => None,
    }
}

fn search_path(node_bin: &Path) -> String {
    let system = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    format!("{}:{system}", node_bin.display())
}

/// A release tag the plugin builds: `vMAJOR.MINOR.PATCH`, nothing else, so it
/// is safe in a URL and a directory name.
pub fn valid_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit())
        })
}

fn agent() -> ureq::Agent {
    let mut agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .user_agent("pom-plugin-ghostly");
    if let Some(proxy) = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .and_then(|value| ureq::Proxy::new(value).ok())
    {
        agent = agent.proxy(proxy);
    }
    agent.build()
}

fn download(url: &str, destination: &Path) -> Result<(), String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|error| format!("download {url}: {error}"))?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("download {url}: {error}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| format!("{}: {error}", destination.display()))?;
    }
    file.sync_all()
        .map_err(|error| format!("{}: {error}", destination.display()))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    std::io::copy(&mut file, &mut hasher)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Unpacks a `.tar.gz`, dropping its single top directory.
fn unpack_stripped(archive: &Path, destination: &Path) -> Result<(), String> {
    let file =
        fs::File::open(archive).map_err(|error| format!("{}: {error}", archive.display()))?;
    let mut unpacker = tar::Archive::new(flate2::read::GzDecoder::new(BufReader::new(file)));
    unpacker.set_preserve_permissions(true);
    for entry in unpacker
        .entries()
        .map_err(|error| format!("read {}: {error}", archive.display()))?
    {
        let mut entry = entry.map_err(|error| format!("read {}: {error}", archive.display()))?;
        let path = entry
            .path()
            .map_err(|error| format!("read {}: {error}", archive.display()))?
            .into_owned();
        let relative: PathBuf = path.components().skip(1).collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if !relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(format!(
                "archive path leaves its directory: {}",
                path.display()
            ));
        }
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        entry
            .unpack(&target)
            .map_err(|error| format!("unpack {}: {error}", relative.display()))?;
    }
    Ok(())
}

/// The pinned Node.js, downloaded and verified once.
pub fn install_node(root: &Path, report: &dyn Fn(&str)) -> Result<PathBuf, String> {
    let platform = platform().ok_or("this platform is not supported by the Ghostly plugin")?;
    let (_, archive_name, checksum) = NODE_ARCHIVES
        .iter()
        .find(|(name, _, _)| *name == platform)
        .ok_or("no Node.js archive for this platform")?;
    let target = root.join("node").join(NODE_VERSION);
    let node = target.join("bin").join("node");
    if target.join(".complete").is_file() && node.is_file() {
        return Ok(node);
    }
    report("node");
    let parent = root.join("node");
    fs::create_dir_all(&parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let archive = parent.join(format!(".{archive_name}"));
    download(
        &format!("https://nodejs.org/dist/{NODE_VERSION}/{archive_name}"),
        &archive,
    )?;
    let digest = sha256_file(&archive)?;
    if !digest.eq_ignore_ascii_case(checksum) {
        let _ = fs::remove_file(&archive);
        return Err("Node.js archive checksum mismatch".into());
    }
    let partial = parent.join(format!(".{NODE_VERSION}.partial"));
    let _ = fs::remove_dir_all(&partial);
    fs::create_dir_all(&partial).map_err(|error| format!("{}: {error}", partial.display()))?;
    let unpacked = unpack_stripped(&archive, &partial);
    let _ = fs::remove_file(&archive);
    unpacked?;
    if !partial.join("bin").join("node").is_file() {
        let _ = fs::remove_dir_all(&partial);
        return Err("Node.js archive has no bin/node".into());
    }
    fs::write(partial.join(".complete"), format!("{checksum}\n"))
        .map_err(|error| format!("mark Node.js: {error}"))?;
    let _ = fs::remove_dir_all(&target);
    fs::rename(&partial, &target).map_err(|error| format!("install Node.js: {error}"))?;
    Ok(node)
}

/// The latest release tag of the original repository.
pub fn latest_tag() -> Result<String, String> {
    let response = agent()
        .get(&format!(
            "https://api.github.com/repos/{GHOSTLY_REPOSITORY}/releases/latest"
        ))
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| format!("GitHub latest release: {error}"))?;
    let body: serde_json::Value = serde_json::from_reader(response.into_reader())
        .map_err(|error| format!("GitHub latest release: {error}"))?;
    let tag = body["tag_name"].as_str().unwrap_or_default();
    if valid_tag(tag) {
        Ok(tag.to_owned())
    } else {
        Err("GitHub returned an unexpected release tag".into())
    }
}

fn cli_entry(prefix: &Path) -> PathBuf {
    prefix
        .join("lib")
        .join("node_modules")
        .join("@ghostlytools")
        .join("cli")
        .join("dist")
        .join("ghostly.mjs")
}

fn cli_ready(prefix: &Path) -> bool {
    prefix.join(".complete").is_file()
        && cli_entry(prefix).is_file()
        && prefix.join("web").join("index.html").is_file()
}

/// Copies a directory tree of regular files and directories.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", from.display()))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("{}: {error}", target.display()))?;
        }
    }
    Ok(())
}

/// Newest complete build, by version, for when GitHub cannot be reached.
fn newest_build(root: &Path) -> Option<String> {
    let mut tags: Vec<(Vec<u32>, String)> = fs::read_dir(root.join("cli"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let tag = entry.file_name().to_str()?.to_owned();
            (valid_tag(&tag) && cli_ready(&entry.path())).then(|| {
                let key = tag[1..]
                    .split('.')
                    .filter_map(|part| part.parse().ok())
                    .collect();
                (key, tag)
            })
        })
        .collect();
    tags.sort();
    tags.pop().map(|(_, tag)| tag)
}

fn run_logged(mut command: Command, log: &Path, what: &str) -> Result<(), String> {
    let output = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|error| format!("{}: {error}", log.display()))?;
    let errors = output
        .try_clone()
        .map_err(|error| format!("{}: {error}", log.display()))?;
    let status = command
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(errors)
        .status()
        .map_err(|error| format!("{what}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status}); see {}", log.display()))
    }
}

/// Downloads the tag's source, builds the CLI and the web app, installs the
/// CLI's package into `cli/<tag>` and copies the web app to `cli/<tag>/web`.
/// Optional native modules (WebRTC) come prebuilt with the install, as they do
/// for `npm install -g @ghostlytools/cli`.
fn build_cli(root: &Path, node: &Path, tag: &str, report: &dyn Fn(&str)) -> Result<(), String> {
    let npm = node
        .parent()
        .map(|bin| bin.join("npm"))
        .ok_or("Node.js has no bin directory")?;
    let build = root.join("build").join(tag);
    let _ = fs::remove_dir_all(&build);
    let source = build.join("source");
    fs::create_dir_all(&source).map_err(|error| format!("{}: {error}", source.display()))?;
    let log = root.join("build.log");
    let _ = fs::write(
        &log,
        format!("building Ghostly {tag} from {GHOSTLY_REPOSITORY}\n"),
    );

    report("download");
    let archive = build.join("source.tar.gz");
    download(
        &format!("https://codeload.github.com/{GHOSTLY_REPOSITORY}/tar.gz/refs/tags/{tag}"),
        &archive,
    )?;
    unpack_stripped(&archive, &source)?;
    let _ = fs::remove_file(&archive);

    let npm_command = |args: &[&str]| {
        let mut command = Command::new(node);
        command
            .arg(&npm)
            .args(args)
            .current_dir(&source)
            .env("PATH", search_path(node.parent().unwrap_or(Path::new("."))))
            .env("npm_config_cache", root.join("npm-cache"))
            .env("npm_config_update_notifier", "false")
            .env("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD", "1")
            .env("CI", "1");
        command
    };
    report("build");
    run_logged(
        npm_command(&["ci", "--ignore-scripts", "--no-audit", "--no-fund"]),
        &log,
        "npm ci",
    )?;
    run_logged(
        npm_command(&["run", "build", "-w", CLI_WORKSPACE]),
        &log,
        "npm run build (CLI)",
    )?;
    let base = format!("--base={WEB_BASE}");
    run_logged(
        npm_command(&["run", "build", "-w", WEB_WORKSPACE, "--", &base]),
        &log,
        "npm run build (web)",
    )?;
    let packed = build.join("pack");
    fs::create_dir_all(&packed).map_err(|error| format!("{}: {error}", packed.display()))?;
    run_logged(
        npm_command(&[
            "pack",
            "-w",
            CLI_WORKSPACE,
            "--pack-destination",
            &packed.to_string_lossy(),
        ]),
        &log,
        "npm pack",
    )?;
    let tarball = fs::read_dir(&packed)
        .map_err(|error| format!("{}: {error}", packed.display()))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "tgz"))
        .ok_or("npm pack produced no package")?;

    report("install");
    let prefix = root.join("cli").join(format!(".{tag}.partial"));
    let _ = fs::remove_dir_all(&prefix);
    fs::create_dir_all(&prefix).map_err(|error| format!("{}: {error}", prefix.display()))?;
    run_logged(
        npm_command(&[
            "install",
            "-g",
            "--no-audit",
            "--no-fund",
            "--prefix",
            &prefix.to_string_lossy(),
            &tarball.to_string_lossy(),
        ]),
        &log,
        "npm install",
    )?;
    if !cli_entry(&prefix).is_file() {
        return Err("the built package has no ghostly.mjs".into());
    }
    let web = source.join("apps").join("web").join("dist");
    if !web.join("index.html").is_file() {
        return Err("the web app build has no index.html".into());
    }
    copy_tree(&web, &prefix.join("web"))?;
    fs::write(prefix.join(".complete"), format!("{tag}\n"))
        .map_err(|error| format!("mark Ghostly: {error}"))?;
    let target = root.join("cli").join(tag);
    let _ = fs::remove_dir_all(&target);
    fs::rename(&prefix, &target).map_err(|error| format!("install Ghostly: {error}"))?;
    let _ = fs::remove_dir_all(root.join("build"));
    Ok(())
}

/// Node.js plus the newest Ghostly CLI and web app, built when needed.
pub fn install(root: &Path, report: &dyn Fn(&str)) -> Result<Runtime, String> {
    let node = install_node(root, report)?;
    report("resolve");
    let tag = match latest_tag() {
        Ok(tag) => {
            if !cli_ready(&root.join("cli").join(&tag)) {
                if let Err(error) = build_cli(root, &node, &tag, report) {
                    // A failed build of a new release keeps the last good one.
                    newest_build(root).ok_or(error)?
                } else {
                    tag
                }
            } else {
                tag
            }
        }
        Err(error) => newest_build(root).ok_or(error)?,
    };
    // Older builds are not needed once the chosen one is in place.
    if let Ok(entries) = fs::read_dir(root.join("cli")) {
        for entry in entries.flatten() {
            if entry.file_name().to_str() != Some(tag.as_str()) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    let prefix = root.join("cli").join(&tag);
    Ok(Runtime {
        node,
        cli: cli_entry(&prefix),
        web: prefix.join("web"),
        version: tag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_release_tags_are_built() {
        assert!(valid_tag("v1.4.5"));
        assert!(valid_tag("v10.0.123"));
        assert!(!valid_tag("1.4.5"));
        assert!(!valid_tag("v1.4"));
        assert!(!valid_tag("v1.4.5-rc1"));
        assert!(!valid_tag("v1.4.5/../x"));
        assert!(!valid_tag("v1..5"));
    }

    #[test]
    fn every_supported_platform_has_a_pinned_node() {
        for (_, name, checksum) in NODE_ARCHIVES {
            assert!(name.contains(NODE_VERSION));
            assert_eq!(checksum.len(), 64);
        }
    }

    #[test]
    fn the_newest_complete_build_wins() {
        let root =
            std::env::temp_dir().join(format!("pom-plugin-ghostly-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (tag, complete) in [("v1.4.5", true), ("v1.10.0", true), ("v2.0.0", false)] {
            let prefix = root.join("cli").join(tag);
            let entry = cli_entry(&prefix);
            fs::create_dir_all(entry.parent().unwrap()).unwrap();
            fs::write(&entry, "").unwrap();
            fs::create_dir_all(prefix.join("web")).unwrap();
            fs::write(prefix.join("web").join("index.html"), "").unwrap();
            if complete {
                fs::write(prefix.join(".complete"), tag).unwrap();
            }
        }
        assert_eq!(newest_build(&root).as_deref(), Some("v1.10.0"));
        fs::remove_dir_all(root).unwrap();
    }
}
