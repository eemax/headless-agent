use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
};

use serde::{Serialize, de::DeserializeOwned};

use crate::error::AppError;

pub fn append_records<T: Serialize>(path: &Path, records: &[T]) -> Result<(), AppError> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    for record in records {
        serde_json::to_writer(&mut file, record)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

pub fn read_records<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, AppError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(&line)?);
    }
    Ok(records)
}
