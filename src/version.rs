use semver::Version;

/// Chrome and Firefox reject manifest version parts above this value.
pub const STORE_MAX_PART: u64 = 65535;

/// Parses a release tag (`v1.2.3`, `1.2.3`, `refs/tags/v1.2.3`) into a version.
///
/// With `strict`, only plain `X.Y.Z` with parts up to [`STORE_MAX_PART`] is
/// accepted: browser extension stores reject pre-release and build suffixes, and
/// stripping them silently would publish a different version than the tag says.
pub fn parse_tag(tag: &str, strict: bool) -> Result<Version, String> {
    let trimmed = tag.trim();
    let bare = trimmed.strip_prefix("refs/tags/").unwrap_or(trimmed);
    let bare = bare.strip_prefix(['v', 'V']).unwrap_or(bare);
    let version = Version::parse(bare)
        .map_err(|err| format!("\"{tag}\" is not a release version (expected vX.Y.Z): {err}"))?;
    if strict {
        if !version.pre.is_empty() || !version.build.is_empty() {
            return Err(format!(
                "\"{tag}\" carries a pre-release or build suffix; --strict allows only X.Y.Z"
            ));
        }
        if [version.major, version.minor, version.patch]
            .iter()
            .any(|part| *part > STORE_MAX_PART)
        {
            return Err(format!(
                "\"{tag}\" has a part above {STORE_MAX_PART}, which browser stores reject"
            ));
        }
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_tag_shapes() {
        for tag in ["v1.2.3", "1.2.3", "V1.2.3", "refs/tags/v1.2.3", " v1.2.3\n"] {
            assert_eq!(
                parse_tag(tag, true).unwrap(),
                Version::new(1, 2, 3),
                "{tag}"
            );
        }
    }

    #[test]
    fn rejects_malformed_tags() {
        for tag in ["v1.2", "v1.2.3.4", "v01.2.3", "release-1", "", "$(id)"] {
            assert!(parse_tag(tag, false).is_err(), "{tag}");
        }
    }

    #[test]
    fn strict_rejects_suffixes_and_large_parts() {
        assert!(parse_tag("v1.2.3-beta.1", true).is_err());
        assert!(parse_tag("v1.2.3+build.5", true).is_err());
        assert!(parse_tag("v70000.0.0", true).is_err());
        assert!(parse_tag("v65535.0.0", true).is_ok());
    }

    #[test]
    fn loose_mode_keeps_suffixes() {
        let version = parse_tag("v2.0.0-rc.1+abc", false).unwrap();
        assert_eq!(version.to_string(), "2.0.0-rc.1+abc");
        assert!(parse_tag("v70000.0.0", false).is_ok());
    }
}
