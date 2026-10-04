use azure_core::prelude::IfMatchCondition;
use azure_core::StatusCode;
use azure_storage::StorageCredentials;
use azure_storage_blobs::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use crate::config::Config;

pub struct StorageService {
    client: Option<Arc<BlobServiceClient>>,
    container_name: String,
    account_name: String,
    /// Development only (`LOCAL_BLOB_DIR`): stands in for Azure when it isn't configured.
    local: Option<LocalBlobs>,
}

struct LocalBlobs {
    dir: PathBuf,
    base_url: String,
}

/// Blob names are relative paths of plain characters, so a local blob can't escape its folder.
pub fn valid_blob_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('/')
        && !name.split('/').any(|part| part.is_empty() || part == "." || part == "..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

impl StorageService {
    pub fn new(cfg: &Config) -> Self {
        let local = (!cfg.local_blob_dir.is_empty()).then(|| LocalBlobs {
            dir: PathBuf::from(&cfg.local_blob_dir),
            base_url: cfg.public_base_url.trim_end_matches('/').to_string(),
        });
        if cfg.azure_conn_str.is_empty() {
            return Self {
                client: None,
                container_name: cfg.container_name.clone(),
                account_name: String::new(),
                local,
            };
        }

        // Simplistic connection string parsing for demo purposes
        let account_name = cfg.azure_conn_str
            .split(';')
            .find(|s| s.starts_with("AccountName="))
            .map(|s| s.trim_start_matches("AccountName=").to_string())
            .unwrap_or_default();

        let account_key = cfg.azure_conn_str
            .split(';')
            .find(|s| s.starts_with("AccountKey="))
            .map(|s| s.trim_start_matches("AccountKey=").to_string())
            .unwrap_or_default();

        if account_name.is_empty() || account_key.is_empty() {
            return Self {
                client: None,
                container_name: cfg.container_name.clone(),
                account_name,
                local,
            };
        }

        let credentials = StorageCredentials::access_key(account_name.clone(), account_key);
        let client = BlobServiceClient::new(account_name.clone(), credentials);

        Self {
            client: Some(Arc::new(client)),
            container_name: cfg.container_name.clone(),
            account_name,
            local: None,
        }
    }

    pub fn is_available(&self) -> bool {
        self.client.is_some()
    }

    /// Whether `upload_blob` can store files: Azure, or the local development folder.
    pub fn can_upload(&self) -> bool {
        self.client.is_some() || self.local.is_some()
    }

    /// Where a local development blob lives on disk; None outside development or for bad names.
    pub fn local_blob_path(&self, blob_name: &str) -> Option<PathBuf> {
        let local = self.local.as_ref()?;
        valid_blob_name(blob_name).then(|| local.dir.join(blob_name))
    }

    pub fn serves_local_blobs(&self) -> bool {
        self.local.is_some()
    }

    pub fn get_container_url(&self) -> String {
        format!("https://{}.blob.core.windows.net/{}", self.account_name, self.container_name)
    }

    pub async fn upload_blob(
        &self,
        data: Vec<u8>,
        blob_name: &str,
        content_type: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        if self.client.is_none() {
            if let Some(local) = &self.local {
                return upload_local(local, data, blob_name).await;
            }
        }
        let client = self.client.as_ref().ok_or("Azure Storage not initialized")?;
        let container_client = client.container_client(&self.container_name);
        let blob_client = container_client.blob_client(blob_name);

        let blob_url = format!(
            "https://{}.blob.core.windows.net/{}/{}",
            self.account_name, self.container_name, blob_name
        );

        // `If-None-Match: *` makes the upload conditional on the blob NOT
        // existing. If it already exists Azure returns 409 Conflict, which we
        // treat as success ("already uploaded, here's the URL"). This is
        // race-safe — two concurrent callers can't both win the upload.
        let result = blob_client
            .put_block_blob(data)
            .content_type(content_type.to_string())
            .if_match(IfMatchCondition::NotMatch("*".to_string()))
            .into_future()
            .await;

        match result {
            Ok(_) => Ok(blob_url),
            Err(e) => {
                if let Some(http_err) = e.as_http_error() {
                    if http_err.status() == StatusCode::Conflict {
                        return Ok(blob_url);
                    }
                }
                Err(Box::new(e))
            }
        }
    }

    pub async fn delete_blobs_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use futures::stream::{self, StreamExt};

        let client = self.client.as_ref().ok_or("Azure Storage not initialized")?;
        let container_client = client.container_client(&self.container_name);

        // Walk paginated list response and collect every blob name.
        let mut names: Vec<String> = Vec::new();
        let mut pages = container_client
            .list_blobs()
            .prefix(prefix.to_string())
            .into_stream();
        while let Some(page) = pages.next().await {
            let page = page?;
            for blob in page.blobs.blobs() {
                names.push(blob.name.clone());
            }
        }

        // Fan out deletes with bounded concurrency. Fail-fast on first error;
        // in-flight deletes that already hit Azure will still complete.
        const PARALLEL: usize = 16;
        let mut results = stream::iter(names)
            .map(|name| {
                let cc = container_client.clone();
                async move {
                    let res = cc.blob_client(&name).delete().into_future().await;
                    (name, res)
                }
            })
            .buffer_unordered(PARALLEL);

        while let Some((name, res)) = results.next().await {
            match res {
                Ok(_) => tracing::info!("Deleted blob: {}", name),
                Err(e) => return Err(Box::new(e)),
            }
        }

        Ok(())
    }

    pub async fn get_blob_content(
        &self,
        blob_name: &str,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let client = self.client.as_ref().ok_or("Azure Storage not initialized")?;
        let container_client = client.container_client(&self.container_name);
        let blob_client = container_client.blob_client(blob_name);

        let data = blob_client.get_content().await?;
        Ok(data)
    }
}

/// Same contract as the Azure upload: the first write wins and later ones return the same URL.
async fn upload_local(
    local: &LocalBlobs,
    data: Vec<u8>,
    blob_name: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    if !valid_blob_name(blob_name) {
        return Err("Invalid blob name".into());
    }
    let path = local.dir.join(blob_name);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, data).await?;
    }
    Ok(format!("{}/dev-blobs/{}", local.base_url, blob_name))
}

#[cfg(test)]
mod local_blob_tests {
    use super::valid_blob_name;

    #[test]
    fn blob_names_stay_inside_their_folder() {
        assert!(valid_blob_name("community/12/01ABC-0f1e2d3c4b5a6978.webp"));
        assert!(!valid_blob_name("../secrets"));
        assert!(!valid_blob_name("community/../../etc/passwd"));
        assert!(!valid_blob_name("/etc/passwd"));
        assert!(!valid_blob_name("community//x.webp"));
        assert!(!valid_blob_name("community/x y.webp"));
        assert!(!valid_blob_name(""));
    }
}
