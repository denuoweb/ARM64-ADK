pub(crate) fn normalize_adb_addr(addr: &str) -> String {
    let addr = addr.trim();
    let lower = addr.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("localhost:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("127.0.0.1:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("0.0.0.0:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("[::1]:") {
        return format!("localhost:{rest}");
    }
    if let Some(rest) = lower.strip_prefix("[::]:") {
        return format!("localhost:{rest}");
    }
    addr.to_string()
}

pub(crate) fn normalize_target_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains(':') {
        return normalize_adb_addr(trimmed);
    }
    trimmed.to_string()
}

pub(crate) fn normalize_target_address(value: &str) -> String {
    normalize_target_id(value)
}

pub(crate) fn normalize_target_id_for_compare(value: &str) -> String {
    normalize_target_id(value).to_ascii_lowercase()
}

pub(crate) fn canonicalize_adb_serial(addr: &str) -> String {
    let addr = addr.trim();
    if let Some(rest) = addr.strip_prefix("localhost:") {
        return format!("127.0.0.1:{rest}");
    }
    if let Some(rest) = addr.strip_prefix("0.0.0.0:") {
        return format!("127.0.0.1:{rest}");
    }
    if let Some(rest) = addr.strip_prefix("[::1]:") {
        return format!("127.0.0.1:{rest}");
    }
    if let Some(rest) = addr.strip_prefix("[::]:") {
        return format!("127.0.0.1:{rest}");
    }
    addr.to_string()
}
