use azure_core::prelude::IfMatchCondition;
use azure_core::StatusCode;
use azure_storage::StorageCredentials;
use azure_storage_blobs::prelude::*;
use std::sync::Arc;
use crate::config::Config;

pub struct StorageService {
    client: Option<Arc<BlobServiceClient>>,
    container_name: String,
    account_name: String,
}

impl StorageService {
    pub fn new(cfg: &Config) -> Self {
        if cfg.azure_conn_str.is_empty() {
            return Self {
                client: None,
                container_name: cfg.container_name.clone(),
                account_name: String::new(),
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
            };
        }

        let credentials = StorageCredentials::access_key(account_name.clone(), account_key);
        let client = BlobServiceClient::new(account_name.clone(), credentials);

        Self {
            client: Some(Arc::new(client)),
            container_name: cfg.container_name.clone(),
            account_name,
        }
    }

    pub fn is_available(&self) -> bool {
        self.client.is_some()
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
        let client = self.client.as_ref().ok_or("Azure Storage not initialized")?;
        let container_client = client.container_client(&self.container_name);

        let mut stream = container_client
            .list_blobs()
            .prefix(prefix.to_string())
            .into_stream();

        while let Some(value) = futures::StreamExt::next(&mut stream).await {
            let resp = value?;
            for blob in resp.blobs.blobs() {
                container_client
                    .blob_client(blob.name.clone())
                    .delete()
                    .into_future()
                    .await?;
                tracing::info!("Deleted blob: {}", blob.name);
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
