//! `update`. Explicit and never automatic: an agent mid-task needs the binary to behave the same
//! from one call to the next, so nothing here runs unless someone asked for it.
//!
//! No Sliplane account is involved. The only host this talks to is the releases page, and it has
//! no access to any API key.

use semver::Version;
use serde_json::Value;

use crate::cli::Update;
use crate::commands::print;
use crate::error::{Error, ErrorCode, Result};
use crate::release::{self, Asset, Releases};
use crate::{obj, replace};

pub fn run(args: Update) -> Result<()> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| Error::other(e.to_string()))?;
    let explicit = args.target_version.as_deref().map(release::parse_version).transpose()?;
    let releases = Releases::from_env();
    let exe = replace::current_exe()?;

    // What to move to, and whether that is a move at all. Without --version the answer is the
    // latest release, and only if it is newer: a build ahead of the last release (a pre-release,
    // or one made from source) is never taken back. --version is the one way to go to an older one.
    let (latest, goal, needed) = match explicit {
        Some(wanted) => {
            let needed = wanted != current;
            (None, wanted, needed)
        }
        None => {
            let latest = releases.latest()?;
            let needed = latest > current;
            (Some(latest.clone()), if needed { latest } else { current.clone() }, needed)
        }
    };

    if args.check || !needed {
        let status = if needed { "update_available" } else { "up_to_date" };
        return print(report(status, &current, &goal, latest.as_ref(), &exe, &releases));
    }

    install(&releases, &current, &goal, latest.as_ref(), &exe)
}

fn install(
    releases: &Releases,
    current: &Version,
    goal: &Version,
    latest: Option<&Version>,
    exe: &std::path::Path,
) -> Result<()> {
    let target = release::target().ok_or_else(|| {
        Error::other(format!("There is no prebuilt release for {}/{}.", std::env::consts::OS, std::env::consts::ARCH))
            .fix(
                "Build it from source: cargo install --git https://github.com/SpaceCorps/Sliplane-Cli --locked --force",
            )
    })?;
    let asset = Asset::new(goal, target);

    // The staged file is created first, so an unwritable install directory fails before any download.
    let staged = replace::Staged::create(exe)?;

    // The checksums are asked for first: a release without them is refused before the archive is fetched.
    let checksums = releases.fetch(goal, "checksums.txt", release::MAX_CHECKSUMS)?.ok_or_else(|| {
        Error::new(ErrorCode::NotFound, format!("v{goal} has no checksums.txt, so its download cannot be verified."))
            .detail(
                "Releases up to 1.2.0 have none, and a release that is still being built gets it last. \
                 sliplane does not install what it cannot verify.",
            )
            .fix(format!(
                "If v{goal} was only just published, wait a few minutes and retry. Otherwise pick a release from 1.3.0 on: \
                 sliplane update --check"
            ))
    })?;
    let expected = release::checksum_for(&String::from_utf8_lossy(&checksums), &asset.archive)?;

    let archive = releases.fetch(goal, &asset.archive, release::MAX_ARCHIVE)?.ok_or_else(|| {
        Error::new(ErrorCode::NotFound, format!("v{goal} has no {} for this platform.", asset.archive))
            .fix("Check the version: sliplane update --check")
    })?;
    let actual = release::sha256_hex(&archive);
    if actual != expected {
        return Err(Error::other(format!(
            "The checksum of {} does not match the one in checksums.txt.",
            asset.archive
        ))
        .detail(format!("expected {expected}, got {actual}"))
        .fix("sliplane was not changed. Retry once; if it fails again, do not install this release."));
    }

    let binary = release::extract_binary(&archive, &asset)?;
    staged.write(&binary, exe)?;
    staged.verify_version(goal)?;
    staged.install(exe)?;

    print(report("updated", current, goal, latest, exe, releases))
}

/// The same envelope for every outcome: what happened, from which version, to which, and where.
fn report(
    status: &str,
    from: &Version,
    to: &Version,
    latest: Option<&Version>,
    exe: &std::path::Path,
    releases: &Releases,
) -> Value {
    let mut out = obj! { "status" => status, "from" => from.to_string(), "to" => to.to_string() };
    let extra = out.as_object_mut().expect("obj! builds an object");
    if let Some(latest) = latest {
        extra.insert("latest".into(), latest.to_string().into());
    }
    extra.insert("path".into(), exe.display().to_string().into());
    if let Some(base) = releases.custom_base() {
        extra.insert("source".into(), base.into());
    }
    if let Some(manager) = replace::managed_by(exe).filter(|_| status != "up_to_date") {
        let note = if status == "updated" {
            format!(
                "This sliplane is installed by {manager}. It was replaced in place; {manager} does not know, and may put its own version back."
            )
        } else {
            format!(
                "This sliplane is installed by {manager}. 'sliplane update' would replace it in place; {manager} would not know."
            )
        };
        extra.insert("note".into(), note.into());
    }
    out
}
