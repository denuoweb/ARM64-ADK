use std::{fs, path::Path};

pub(crate) fn parse_list_tokens(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_string())
        .collect()
}

pub(crate) fn infer_application_id_from_project(project_path: &str) -> Option<String> {
    let trimmed = project_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let root = Path::new(trimmed);
    if !root.is_dir() {
        return None;
    }

    let gradle_candidates = [
        root.join("app").join("build.gradle.kts"),
        root.join("app").join("build.gradle"),
    ];
    for candidate in gradle_candidates {
        if let Ok(contents) = fs::read_to_string(candidate) {
            if let Some(value) = parse_gradle_value(&contents, "applicationId") {
                return Some(value);
            }
            if let Some(value) = parse_gradle_value(&contents, "namespace") {
                return Some(value);
            }
        }
    }

    let manifest_path = root
        .join("app")
        .join("src")
        .join("main")
        .join("AndroidManifest.xml");
    if let Ok(contents) = fs::read_to_string(manifest_path) {
        if let Some(value) = parse_manifest_package(&contents) {
            return Some(value);
        }
    }

    None
}

fn parse_gradle_value(contents: &str, key: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        let rest = match trimmed.strip_prefix(key) {
            Some(rest) => rest,
            None => continue,
        };
        if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()) || rest.starts_with('=')
        {
            if let Some(value) = extract_quoted_value(rest) {
                return Some(value);
            }
        }
    }
    None
}

fn parse_manifest_package(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(idx) = line.find("package") {
            if let Some(value) = extract_quoted_value(&line[idx..]) {
                return Some(value);
            }
        }
    }
    None
}

fn extract_quoted_value(raw: &str) -> Option<String> {
    let mut quote = None;
    let mut start = 0;
    for (idx, ch) in raw.char_indices() {
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            start = idx + ch.len_utf8();
            break;
        }
    }
    let quote = quote?;
    let rest = &raw[start..];
    let end = rest.find(quote)?;
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
