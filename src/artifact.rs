use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{error::AppError, types::ArtifactRef};

#[derive(Debug, Clone)]
pub struct StoredText {
    pub transcript_text: String,
    pub preview: String,
    pub artifact: Option<ArtifactRef>,
}

pub fn store_text_artifact(
    run_dir: &Path,
    relative_dir: &str,
    file_name: &str,
    content: &str,
    preview_bytes: usize,
    catastrophic_output_bytes: usize,
    content_kind: Option<&str>,
) -> Result<StoredText, AppError> {
    let bytes = content.as_bytes().len();
    let preview = preview_text(content, preview_bytes);
    if bytes <= preview_bytes {
        return Ok(StoredText {
            transcript_text: content.to_string(),
            preview,
            artifact: None,
        });
    }

    let artifact_dir = run_dir.join(relative_dir);
    fs::create_dir_all(&artifact_dir)?;
    let artifact_path = artifact_dir.join(file_name);
    fs::write(&artifact_path, content)?;
    let relative_path = artifact_path
        .strip_prefix(run_dir)
        .unwrap_or(&artifact_path)
        .to_string_lossy()
        .to_string();

    let transcript_text = if bytes > catastrophic_output_bytes {
        format!("[artifact stored at {relative_path} ({bytes} bytes); inline content omitted]")
    } else {
        preview.clone()
    };

    Ok(StoredText {
        transcript_text,
        preview,
        artifact: Some(ArtifactRef {
            path: relative_path,
            bytes: bytes as u64,
            sha256: None,
            content_kind: content_kind.map(ToOwned::to_owned),
        }),
    })
}

pub fn create_run_dir(session_dir: &Path, run_id: &str) -> Result<PathBuf, AppError> {
    let path = session_dir.join("runs").join(run_id);
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn preview_text(content: &str, limit: usize) -> String {
    if content.as_bytes().len() <= limit {
        return content.to_string();
    }
    let mut end = limit.min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n...[truncated {} bytes]",
        &content[..end],
        content.as_bytes().len().saturating_sub(end)
    )
}
