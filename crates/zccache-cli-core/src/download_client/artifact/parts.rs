use std::path::Path;

use tokio::io::AsyncWriteExt;
use zccache_download::http::header::ACCEPT_ENCODING;
use zccache_download::http::{self, HttpClient};

use super::hashing::temp_download_path;

pub(super) fn download_explicit_parts(
    part_urls: &[String],
    destination: &Path,
) -> Result<(), String> {
    let temp_path = temp_download_path(destination);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to create tokio runtime: {e}"))?;
    crate::core::wait::block_on_for(
        &runtime,
        async move {
            let client = HttpClient::new(
                &format!("zccache-download/{}", crate::core::VERSION),
                false,
                http::CONNECT_BUDGET,
            )
            .map_err(|e| e.to_string())?;
            let cancel = http::uncancelled();

            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }

            let _ = tokio::fs::remove_file(&temp_path).await;

            let result = async {
                let mut output = tokio::fs::File::create(&temp_path)
                    .await
                    .map_err(|e| e.to_string())?;
                for url in part_urls {
                    let mut response = client
                        .get(url, http::TRANSFER)
                        .header(ACCEPT_ENCODING, "identity")
                        .send(&cancel)
                        .await
                        .map_err(|e| e.to_string())?;
                    if !response.status().is_success() {
                        return Err(format!(
                            "unexpected status {} for {}",
                            response.status(),
                            http::redact_url(url)
                        ));
                    }
                    while let Some(chunk) =
                        response.next_chunk().await.map_err(|e| e.to_string())?
                    {
                        output
                            .write_all(chunk.as_ref())
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                }
                output.flush().await.map_err(|e| e.to_string())?;
                drop(output);
                if destination.exists() {
                    let _ = tokio::fs::remove_file(destination).await;
                }
                tokio::fs::rename(&temp_path, destination)
                    .await
                    .map_err(|e| e.to_string())
            }
            .await;

            if result.is_err() {
                let _ = tokio::fs::remove_file(&temp_path).await;
            }
            result
        },
        crate::core::wait::WHOLE_COMMAND,
    )
    .map_err(|error| error.to_string())?
}
