//! User-facing person names for Chief-owned launch surfaces.

/// The explicit role when a roster row has no usable job title.
pub const TEAM_MEMBER_ROLE: &str = "Team member";
/// The CEO's invariant product role.
pub const CEO_ROLE: &str = "Chief Executive Officer";

/// The first human name in the authoritative roster display name.
#[must_use]
pub fn first_name(display_name: &str) -> String {
    display_name.split_whitespace().next().unwrap_or("Person").to_owned()
}

/// The stable short identity shown by Pi and Chief pane surfaces.
#[must_use]
pub fn short_identity(display_name: &str) -> String {
    let slug: String = first_name(display_name)
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric() || *character == '_' || *character == '-')
        .collect();
    format!("@{}", if slug.is_empty() { "person" } else { slug.as_str() })
}

/// The real roster role, or the explicit product default.
#[must_use]
pub fn role(display_name: &str, title: &str, is_ceo: bool) -> String {
    if is_ceo {
        return CEO_ROLE.to_owned();
    }
    let title = title.trim();
    if title.is_empty()
        || title.eq_ignore_ascii_case(display_name.trim())
        || title.eq_ignore_ascii_case(&first_name(display_name))
    {
        TEAM_MEMBER_ROLE.to_owned()
    } else {
        title.to_owned()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn presentation_uses_first_name_short_identity_and_a_real_role() {
        assert_eq!(super::first_name("Vera Jones"), "Vera");
        assert_eq!(super::short_identity("Vera Jones"), "@vera");
        assert_eq!(super::role("Vera Jones", "Test Engineer", false), "Test Engineer");
        assert_eq!(super::role("Vera Jones", "Vera Jones", false), "Team member");
        assert_eq!(super::role("Avery", "", true), "Chief Executive Officer");
    }
}
