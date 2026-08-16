use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use futures::StreamExt;
use gpui::AsyncApp;
use http_client::github::{AssetKind, GitHubLspBinaryVersion, get_release_by_tag_name};
use http_client::github_download::{GithubBinaryMetadata, download_server_binary};
use language::{LspAdapter, LspAdapterDelegate, LspInstaller, Toolchain};
use lsp::{LanguageServerBinary, LanguageServerName};
use smol::fs;
use std::{env::consts, future::Future, path::PathBuf, sync::Arc};
use util::{fs::remove_matching, maybe};

/// Markdown-oxide provides Suzuri's Obsidian-grade PKM features — wikilink
/// completion and following, backlinks as references, broken-link
/// diagnostics — provisioned as a plain LSP binary so none of its code or
/// license enters the build, and replaceable behind the protocol boundary.
///
/// Pinned to a vetted release rather than tracking latest; bump the tag
/// deliberately after trying the new version. A PATH-installed
/// `markdown-oxide` (brew, cargo install) always takes precedence.
const PINNED_RELEASE_TAG: &str = "v0.25.12";
const REPOSITORY: &str = "Feel-ix-343/markdown-oxide";

pub struct MarkdownOxideLspAdapter;

impl MarkdownOxideLspAdapter {
    const SERVER_NAME: LanguageServerName = LanguageServerName::new_static("markdown-oxide");

    /// Release asset stem for this platform, e.g.
    /// `markdown-oxide-v0.25.12-aarch64-apple-darwin`. The archive extracts
    /// to a directory of the same name containing the binary.
    fn asset_stem() -> Result<String> {
        let arch = match consts::ARCH {
            "aarch64" => "aarch64",
            "x86_64" => "x86_64",
            other => bail!("markdown-oxide has no prebuilt binary for architecture {other}"),
        };
        let os = match (consts::OS, arch) {
            ("macos", _) => "apple-darwin",
            ("linux", _) => "unknown-linux-gnu",
            ("windows", "x86_64") => "pc-windows-gnu",
            (os, arch) => bail!("markdown-oxide has no prebuilt binary for {os} on {arch}"),
        };
        Ok(format!("markdown-oxide-{PINNED_RELEASE_TAG}-{arch}-{os}"))
    }

    fn asset_kind() -> AssetKind {
        if consts::OS == "windows" {
            AssetKind::Zip
        } else {
            AssetKind::TarGz
        }
    }

    fn asset_kind_extension() -> &'static str {
        match Self::asset_kind() {
            AssetKind::Zip => "zip",
            _ => "tar.gz",
        }
    }

    fn binary_in(version_dir: &std::path::Path) -> PathBuf {
        version_dir.join(format!("markdown-oxide{}", consts::EXE_SUFFIX))
    }
}

impl LspInstaller for MarkdownOxideLspAdapter {
    type BinaryVersion = GitHubLspBinaryVersion;

    async fn fetch_latest_server_version(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _pre_release: bool,
        _: &mut AsyncApp,
    ) -> Result<GitHubLspBinaryVersion> {
        let release =
            get_release_by_tag_name(REPOSITORY, PINNED_RELEASE_TAG, delegate.http_client())
                .await?;
        let asset_name = format!(
            "{}.{}",
            Self::asset_stem()?,
            Self::asset_kind_extension()
        );
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .with_context(|| format!("no asset found matching {asset_name:?}"))?;
        Ok(GitHubLspBinaryVersion {
            name: release.tag_name,
            url: asset.browser_download_url.clone(),
            digest: asset.digest.as_deref().map(normalize_digest),
        })
    }

    async fn check_if_user_installed(
        &self,
        delegate: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: &AsyncApp,
    ) -> Option<LanguageServerBinary> {
        let path = delegate.which(Self::SERVER_NAME.as_ref()).await?;
        Some(LanguageServerBinary {
            path,
            arguments: Vec::new(),
            env: None,
        })
    }

    fn fetch_server_binary(
        &self,
        version: GitHubLspBinaryVersion,
        container_dir: PathBuf,
        delegate: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Result<LanguageServerBinary>> + use<> {
        let delegate = delegate.clone();

        async move {
            let GitHubLspBinaryVersion {
                url,
                digest: expected_digest,
                ..
            } = version;
            let version_dir = container_dir.join(Self::asset_stem()?);
            let binary_path = Self::binary_in(&version_dir);

            let binary = LanguageServerBinary {
                path: binary_path.clone(),
                env: None,
                arguments: Vec::new(),
            };

            let metadata_path = version_dir.join("metadata");
            let metadata = GithubBinaryMetadata::read_from_file(&metadata_path)
                .await
                .ok();
            if let Some(metadata) = metadata {
                let validity_check = async || {
                    delegate
                        .try_exec(LanguageServerBinary {
                            path: binary_path.clone(),
                            arguments: vec!["--version".into()],
                            env: None,
                        })
                        .await
                        .inspect_err(|err| {
                            log::warn!(
                                "Unable to run {binary_path:?} asset, redownloading: {err:#}",
                            )
                        })
                };
                if let (Some(actual_digest), Some(expected_digest)) =
                    (&metadata.digest, &expected_digest)
                {
                    if actual_digest == expected_digest {
                        if validity_check().await.is_ok() {
                            return Ok(binary);
                        }
                    } else {
                        log::info!(
                            "SHA-256 mismatch for {binary_path:?} asset, downloading new asset. Expected: {expected_digest}, Got: {actual_digest}"
                        );
                    }
                } else if validity_check().await.is_ok() {
                    return Ok(binary);
                }
            }
            download_server_binary(
                &*delegate.http_client(),
                &url,
                expected_digest.as_deref(),
                &container_dir,
                Self::asset_kind(),
            )
            .await?;
            remove_matching(&container_dir, |entry| entry != version_dir).await;
            GithubBinaryMetadata::write_to_file(
                &GithubBinaryMetadata {
                    metadata_version: 1,
                    digest: expected_digest,
                },
                &metadata_path,
            )
            .await?;

            Ok(binary)
        }
    }

    async fn cached_server_binary(
        &self,
        container_dir: PathBuf,
        _: &dyn LspAdapterDelegate,
    ) -> Option<LanguageServerBinary> {
        maybe!(async {
            let mut last_version_dir = None;
            let mut entries = fs::read_dir(&container_dir).await?;
            while let Some(entry) = entries.next().await {
                let entry = entry?;
                if entry.file_type().await?.is_dir() {
                    last_version_dir = Some(entry.path());
                }
            }
            let version_dir = last_version_dir.context("no cached binary")?;
            let binary_path = Self::binary_in(&version_dir);
            anyhow::ensure!(
                binary_path.exists(),
                "missing markdown-oxide binary in directory {version_dir:?}"
            );
            Ok(LanguageServerBinary {
                path: binary_path,
                env: None,
                arguments: Vec::new(),
            })
        })
        .await
        .ok()
    }
}

#[async_trait(?Send)]
impl LspAdapter for MarkdownOxideLspAdapter {
    fn name(&self) -> LanguageServerName {
        Self::SERVER_NAME
    }
}

/// The GitHub API returns digests as `sha256:<hex>`. `latest_github_release`
/// strips that prefix but `get_release_by_tag_name` does not, and the
/// downloader compares against bare hex — without this the digest check
/// always fails.
fn normalize_digest(digest: &str) -> String {
    digest.strip_prefix("sha256:").unwrap_or(digest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the asset naming contract with markdown-oxide's releases: stem
    /// is `markdown-oxide-<tag>-<arch>-<os>`, archives add `.tar.gz` (unix)
    /// or `.zip` (windows), and the binary lives inside a directory named
    /// after the stem. If a version bump changes this layout, fail here
    /// instead of at first download.
    #[test]
    fn test_digest_normalization() {
        assert_eq!(normalize_digest("sha256:abc123"), "abc123");
        assert_eq!(normalize_digest("abc123"), "abc123");
    }

    #[test]
    fn test_asset_naming_contract() {
        let stem = MarkdownOxideLspAdapter::asset_stem().unwrap();
        assert!(stem.starts_with("markdown-oxide-v"), "{stem}");
        assert!(
            stem.ends_with("apple-darwin")
                || stem.ends_with("unknown-linux-gnu")
                || stem.ends_with("pc-windows-gnu"),
            "{stem}"
        );
        let binary = MarkdownOxideLspAdapter::binary_in(std::path::Path::new("dir"));
        assert!(
            binary
                .to_string_lossy()
                .starts_with("dir/markdown-oxide")
                || binary.to_string_lossy().starts_with("dir\\markdown-oxide"),
            "{binary:?}"
        );
    }
}
