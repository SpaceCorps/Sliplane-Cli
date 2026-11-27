//! The sizes the API's catalog accepts, as spec 0.13.0 lists them (`InstanceType`,
//! `PostgresInstanceType`, and the `diskSizeGb` enums). Help text and the autoscaling checks both
//! read them from here, so the two cannot drift apart.

/// Compute tiers, shared first, then dedicated. Servers and Postgres use the same names. Within a
/// family each tier has more memory than the one before it, and never fewer vCPUs, so the order is
/// the size.
pub const INSTANCE_TYPES: &[&str] = &[
    "starter",
    "base",
    "medium",
    "large",
    "x-large",
    "xx-large",
    "dedicated-base",
    "dedicated-medium",
    "dedicated-large",
    "dedicated-x-large",
    "dedicated-xx-large",
    "dedicated-xxx-large",
];

/// Server disk sizes in GB beyond the 20 GB every instance type bundles.
pub const SERVER_DISK_GB: &[u64] = &[50, 100, 250, 500, 1000];

/// Postgres disk sizes in GB; 10 is the one bundled with every tier.
pub const POSTGRES_DISK_GB: &[u64] = &[10, 50, 100, 250, 500, 1000];

/// `starter, base, ...` for help text and errors.
pub fn instance_types() -> String {
    INSTANCE_TYPES.join(", ")
}

/// `50, 100, ...` for help text and errors.
pub fn sizes(list: &[u64]) -> String {
    list.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
}

/// A tier's place in the catalog, or `None` for a name that is not in it.
pub fn tier(name: &str) -> Option<usize> {
    INSTANCE_TYPES.iter().position(|t| *t == name)
}

/// Shared and dedicated tiers are separate CPU families; autoscaling stays inside one.
pub fn family(name: &str) -> &'static str {
    if name.starts_with("dedicated-") { "dedicated" } else { "shared" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_family_is_listed_smallest_first() {
        let shared: Vec<_> = INSTANCE_TYPES.iter().filter(|t| family(t) == "shared").collect();
        let dedicated: Vec<_> = INSTANCE_TYPES.iter().filter(|t| family(t) == "dedicated").collect();
        assert_eq!(shared, ["starter", "base", "medium", "large", "x-large", "xx-large"].iter().collect::<Vec<_>>());
        assert_eq!(dedicated.len(), 6);
        assert!(tier("large") < tier("x-large") && tier("dedicated-base") < tier("dedicated-xxx-large"));
        assert_eq!(tier("huge"), None);
    }

    #[test]
    fn lists_read_as_help_text() {
        assert!(instance_types().starts_with("starter, base, medium"));
        assert_eq!(sizes(SERVER_DISK_GB), "50, 100, 250, 500, 1000");
        assert_eq!(sizes(POSTGRES_DISK_GB), "10, 50, 100, 250, 500, 1000");
    }
}
