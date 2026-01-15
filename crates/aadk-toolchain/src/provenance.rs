use std::{fs, io, path::Path};

#[derive(Clone, Default)]
pub(crate) struct Provenance {
    pub(crate) provider_id: String,
    pub(crate) provider_name: String,
    pub(crate) version: String,
    pub(crate) source_url: String,
    pub(crate) sha256: String,
    pub(crate) installed_at_unix_millis: i64,
    pub(crate) cached_path: String,
    pub(crate) host: String,
    pub(crate) artifact_size_bytes: u64,
    pub(crate) signature: String,
    pub(crate) signature_url: String,
    pub(crate) signature_public_key: String,
    pub(crate) transparency_log_entry: String,
    pub(crate) transparency_log_entry_url: String,
    pub(crate) transparency_log_public_key: String,
}

pub(crate) fn write_provenance(dir: &Path, prov: &Provenance) -> io::Result<()> {
    let mut contents = format!(
        "provider_id={}\nprovider_name={}\nversion={}\nsource_url={}\nsha256={}\ninstalled_at_unix_millis={}\n",
        prov.provider_id,
        prov.provider_name,
        prov.version,
        prov.source_url,
        prov.sha256,
        prov.installed_at_unix_millis
    );
    if !prov.cached_path.is_empty() {
        contents.push_str(&format!("cached_path={}\n", prov.cached_path));
    }
    if !prov.host.is_empty() {
        contents.push_str(&format!("host={}\n", prov.host));
    }
    if prov.artifact_size_bytes > 0 {
        contents.push_str(&format!(
            "artifact_size_bytes={}\n",
            prov.artifact_size_bytes
        ));
    }
    if !prov.signature.is_empty() {
        contents.push_str(&format!("signature={}\n", prov.signature));
    }
    if !prov.signature_url.is_empty() {
        contents.push_str(&format!("signature_url={}\n", prov.signature_url));
    }
    if !prov.signature_public_key.is_empty() {
        contents.push_str(&format!(
            "signature_public_key={}\n",
            prov.signature_public_key
        ));
    }
    if !prov.transparency_log_entry.is_empty() {
        contents.push_str(&format!(
            "transparency_log_entry={}\n",
            prov.transparency_log_entry
        ));
    }
    if !prov.transparency_log_entry_url.is_empty() {
        contents.push_str(&format!(
            "transparency_log_entry_url={}\n",
            prov.transparency_log_entry_url
        ));
    }
    if !prov.transparency_log_public_key.is_empty() {
        contents.push_str(&format!(
            "transparency_log_public_key={}\n",
            prov.transparency_log_public_key
        ));
    }
    fs::write(dir.join("provenance.txt"), contents)
}

fn parse_provenance(contents: &str) -> Provenance {
    let mut prov = Provenance::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "provider_id" => prov.provider_id = value.to_string(),
            "provider_name" => prov.provider_name = value.to_string(),
            "version" => prov.version = value.to_string(),
            "source_url" => prov.source_url = value.to_string(),
            "sha256" => prov.sha256 = value.to_string(),
            "installed_at_unix_millis" => {
                if let Ok(parsed) = value.parse::<i64>() {
                    prov.installed_at_unix_millis = parsed;
                }
            }
            "cached_path" => prov.cached_path = value.to_string(),
            "host" => prov.host = value.to_string(),
            "artifact_size_bytes" => {
                if let Ok(parsed) = value.parse::<u64>() {
                    prov.artifact_size_bytes = parsed;
                }
            }
            "signature" => prov.signature = value.to_string(),
            "signature_url" => prov.signature_url = value.to_string(),
            "signature_public_key" => prov.signature_public_key = value.to_string(),
            "transparency_log_entry" => prov.transparency_log_entry = value.to_string(),
            "transparency_log_entry_url" => prov.transparency_log_entry_url = value.to_string(),
            "transparency_log_public_key" => prov.transparency_log_public_key = value.to_string(),
            _ => {}
        }
    }
    prov
}

pub(crate) fn read_provenance(path: &Path) -> io::Result<Provenance> {
    let contents = fs::read_to_string(path)?;
    Ok(parse_provenance(&contents))
}
