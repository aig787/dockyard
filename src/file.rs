use anyhow::{Context, Result};
use std::path::Path;
use std::fs::File;
use std::io::Write;
use std::fs;

pub fn write_file(contents: &str, output: &str) -> Result<()> {
    log::debug!("Writing contents to {}", output);
    let output_path = Path::new(output);
    fs::create_dir_all(output_path.parent().unwrap())?;
    let mut output_file = File::create(output_path)?;
    output_file.write_all(contents.as_bytes())?;
    Ok(())
}

pub fn decode_and_write_file(contents: &str, output: &str) -> Result<()> {
    log::debug!("Decoding input as base64");
    write_file(&decode_b64(contents)?, output)
}

pub fn decode_b64(contents: &str) -> Result<String> {
    Ok(base64::decode(contents)?.into_iter().map(|i| i as char).collect())
}

pub fn read_file(path: &str) -> Result<String> {
    log::debug!("Reading {}", path);
    match fs::read_to_string(Path::new(path)) {
        Ok(s) => Ok(s),
        Err(e) => Err(e).with_context(|| "Failed to read file")
    }
}

pub fn read_and_encode_file(path: &str) -> Result<String> {
    let contents = read_file(path)?;
    log::debug!("Encoding contents of {} as base64", path);
    Ok(base64::encode(contents))
}


#[cfg(test)]
mod test {
    use super::*;
    use simple_logger::SimpleLogger;
    use tempfile::TempDir;
    use log::LevelFilter;
    use rand::{Rng, thread_rng};
    use rand::distributions::Alphanumeric;
    use std::fs;

    #[test]
    fn write_file_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let working_dir = TempDir::new().unwrap();
        let output = working_dir.path().join("out");
        let contents = rand_string();
        write_file(&contents, output.as_path().to_str().unwrap()).unwrap();
        let written_contents = fs::read_to_string(output).unwrap();
        assert_eq!(written_contents, contents);
    }

    #[test]
    fn write_encoded_test() {
        let _ = SimpleLogger::new().with_level(LevelFilter::Info).init();
        let working_dir = TempDir::new().unwrap();
        let output = working_dir.path().join("out");
        let contents = rand_string();
        decode_and_write_file(&base64::encode(&contents), output.as_path().to_str().unwrap()).unwrap();
        let written_contents = fs::read_to_string(output).unwrap();
        assert_eq!(written_contents, contents);
    }

    fn rand_string() -> String {
        thread_rng()
            .sample_iter(&Alphanumeric)
            .take(30)
            .collect()
    }
}
