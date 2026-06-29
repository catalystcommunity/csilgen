//! Shared helpers for deriving ecosystem package names from configured options.

/// The ecosystem package name carried by a configured `package_name`: its last path
/// segment.
///
/// `package_name` is a single global option but doubles as a path-style coordinate —
/// the Go generator already treats it as a module path (`github.com/org/repo/clients/foo`)
/// and derives its bare `package` clause from the tail. Every other ecosystem wants a
/// bare name too (`foo` on npm/PyPI/crates.io/RubyGems/…), so one path-style value can
/// serve as the source of truth: Go consumes the whole path for its module line while
/// everyone else consumes the tail. A slash-free value (`longhouse`) is returned
/// unchanged, preserving today's behavior. Trailing slashes are ignored so `a/b/`
/// yields `b`. Sanitizing the result to a given ecosystem's identifier rules remains
/// the caller's job — this only selects which substring to sanitize.
pub fn package_name_last_segment(package_name: &str) -> &str {
    package_name
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(package_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_name_is_unchanged() {
        assert_eq!(package_name_last_segment("longhouse"), "longhouse");
        assert_eq!(
            package_name_last_segment("corndogs-client"),
            "corndogs-client"
        );
    }

    #[test]
    fn path_collapses_to_tail() {
        assert_eq!(
            package_name_last_segment("github.com/org/repo/clients/corndogs"),
            "corndogs"
        );
        assert_eq!(package_name_last_segment("a/b/c"), "c");
    }

    #[test]
    fn trailing_slashes_ignored() {
        assert_eq!(
            package_name_last_segment("github.com/org/corndogs/"),
            "corndogs"
        );
        assert_eq!(package_name_last_segment("a/b/"), "b");
    }

    #[test]
    fn degenerate_inputs() {
        assert_eq!(package_name_last_segment(""), "");
        assert_eq!(package_name_last_segment("/"), "");
    }
}
