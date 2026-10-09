use uuid::Uuid;

pub fn normalize_email(value: &str) -> Result<String, &'static str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("email required");
    }
    Ok(trimmed.to_string())
}

pub fn validate_password(value: &str, allow_empty: bool) -> Result<(), &'static str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        if allow_empty {
            Ok(())
        } else {
            Err("password required")
        }
    } else if trimmed.len() < 8 {
        Err("password must be at least 8 characters")
    } else {
        Ok(())
    }
}

pub fn parse_uuid_list(values: &[String]) -> Vec<Uuid> {
    values
        .iter()
        .filter_map(|val| Uuid::parse_str(val.trim()).ok())
        .collect()
}
