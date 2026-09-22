//! Catches path arguments that a Unix-style shell on Windows has rewritten.
//!
//! Git Bash and MSYS translate arguments that look like absolute Unix paths into Windows ones
//! before the process ever sees them, so `--healthcheck /` arrives as "C:/Program Files/Git/".
//! Setting MSYS_NO_PATHCONV=1 does not help, because the rewriting happens in the shell.
//!
//! The result deploys quite happily and then fails every healthcheck, with nothing in the
//! output to suggest why. Better to refuse up front.

use crate::error::{Error, Result};

pub fn check<'a>(value: &'a str, option: &str) -> Result<&'a str> {
    // A URL path never contains a drive-letter separator.
    if value.contains(":/") || value.contains(":\\") {
        return Err(Error::invalid(format!(
            "{option} was given \"{value}\", which is a Windows path rather than a URL path.\n\n\
             A Unix-style shell on Windows rewrites arguments starting with \"/\" before this \
             program sees them, turning \"/\" into your Git installation directory. \
             MSYS_NO_PATHCONV does not prevent it.\n\n\
             Run this from PowerShell or cmd, or double the slash: //health"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn accepts_url_paths() {
        assert!(check("/", "--healthcheck").is_ok());
        assert!(check("/health?x=1", "--healthcheck").is_ok());
    }

    #[test]
    fn refuses_rewritten_paths() {
        assert!(check("C:/Program Files/Git/", "--healthcheck").is_err());
        assert!(check("C:\\Program Files\\Git\\health", "--healthcheck").is_err());
    }
}
