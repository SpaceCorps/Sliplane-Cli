//! `--image` shorthand. People write `nginx`, `nginx:1.27` or `user/app`; the API's own example
//! is `docker.io/library/nginx:latest` and the official CLI sends that form (it expands the
//! same shorthand), so that is what we send for Docker Hub references.
//!
//! The rules are the ones Docker applies, and no more:
//!
//! - The first path component is a registry host when it contains `.` or `:` or is `localhost`.
//!   Then the reference is already complete and is left exactly as written. (`docker.io` with a
//!   single path component, as in `docker.io/nginx`, is the one exception: Docker Hub keeps its
//!   official images under `library/`.)
//! - Otherwise it is Docker Hub: `nginx` is `docker.io/library/nginx`, `user/app` is
//!   `docker.io/user/app`.
//! - A rewritten reference with neither tag nor digest gets `:latest`, which is what Docker
//!   would have pulled anyway. A reference we leave alone is never given one.
//! - A digest (`@sha256:...`) is kept and is never combined with `:latest`.

/// The reference to send to the API for what the user typed.
pub fn normalize(image: &str) -> String {
    // Anything that cannot be a reference is the API's to refuse; do not make it worse.
    if image.is_empty()
        || image.starts_with('/')
        || image.ends_with('/')
        || image.contains("//")
        || image.contains(char::is_whitespace)
    {
        return image.to_string();
    }

    // A digest ends the reference, so look for the tag only before it.
    let (name, digest) = match image.split_once('@') {
        Some((name, digest)) => (name, Some(digest)),
        None => (image, None),
    };

    let path = match name.split_once('/') {
        Some((host, rest)) if host.contains('.') || host.contains(':') || host == "localhost" => {
            if host != "docker.io" || rest.contains('/') {
                return image.to_string();
            }
            rest
        }
        _ => name,
    };

    let path = if path.contains('/') { path.to_string() } else { format!("library/{path}") };
    let has_tag = path.rsplit('/').next().is_some_and(|last| last.contains(':'));
    let tag = if digest.is_none() && !has_tag { ":latest" } else { "" };
    let digest = digest.map(|d| format!("@{d}")).unwrap_or_default();
    format!("docker.io/{path}{tag}{digest}")
}

/// `normalize`, saying so on stderr when the value changed.
pub fn normalize_noted(image: &str) -> String {
    let full = normalize(image);
    if full != image {
        eprintln!(
            "note: --image '{image}' was sent as '{full}' (Docker Hub). Use a full reference such as ghcr.io/org/app:tag for any other registry."
        );
    }
    full
}

#[cfg(test)]
mod tests {
    use super::normalize;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn same(image: &str) {
        assert_eq!(normalize(image), image, "{image} is complete and must not change");
    }

    #[test]
    fn official_images_move_under_library() {
        assert_eq!(normalize("nginx"), "docker.io/library/nginx:latest");
        assert_eq!(normalize("nginx:1.27"), "docker.io/library/nginx:1.27");
        assert_eq!(normalize("nginx:latest"), "docker.io/library/nginx:latest");
        assert_eq!(normalize("library/nginx"), "docker.io/library/nginx:latest");
        assert_eq!(normalize("library/nginx:alpine"), "docker.io/library/nginx:alpine");
        assert_eq!(normalize("docker.io/nginx"), "docker.io/library/nginx:latest");
        assert_eq!(normalize("docker.io/nginx:1.27"), "docker.io/library/nginx:1.27");
    }

    #[test]
    fn user_images_get_the_docker_hub_host() {
        assert_eq!(normalize("user/app"), "docker.io/user/app:latest");
        assert_eq!(normalize("user/app:v2"), "docker.io/user/app:v2");
        assert_eq!(normalize("a/b/c"), "docker.io/a/b/c:latest");
    }

    #[test]
    fn a_reference_with_a_registry_host_is_left_alone() {
        same("docker.io/library/nginx");
        same("docker.io/library/nginx:1.27");
        same("docker.io/user/app:v2");
        same("ghcr.io/org/repo:tag");
        same("ghcr.io/org/repo");
        same("quay.io/org/repo/sub:1");
        same("localhost:5000/app");
        same("localhost:5000/app:v1");
        same("localhost/app");
        same("registry.example:5000/team/web");
        same("registry.example:5000/team/web:v1");
        same("registry.example:5000/web");
        same("123456789.dkr.ecr.eu-west-1.amazonaws.com/app:1");
        same("index.docker.io/library/nginx");
    }

    #[test]
    fn a_port_is_not_mistaken_for_a_tag() {
        // `host:port/app` has a ':' in the first component; the tag is looked for in the last.
        same("localhost:5000/app");
        assert_eq!(normalize("user/app"), "docker.io/user/app:latest");
        // Without a path, `name:5000` is a tag, as Docker reads it.
        assert_eq!(normalize("redis:5000"), "docker.io/library/redis:5000");
    }

    #[test]
    fn digests_are_kept_and_never_get_latest() {
        assert_eq!(normalize(&format!("nginx@{DIGEST}")), format!("docker.io/library/nginx@{DIGEST}"));
        assert_eq!(normalize(&format!("nginx:1.27@{DIGEST}")), format!("docker.io/library/nginx:1.27@{DIGEST}"));
        assert_eq!(normalize(&format!("user/app@{DIGEST}")), format!("docker.io/user/app@{DIGEST}"));
        assert_eq!(normalize(&format!("docker.io/nginx@{DIGEST}")), format!("docker.io/library/nginx@{DIGEST}"));
        same(&format!("ghcr.io/o/app@{DIGEST}"));
        same(&format!("localhost:5000/app@{DIGEST}"));
        same(&format!("docker.io/library/nginx@{DIGEST}"));
    }

    #[test]
    fn it_is_idempotent() {
        for image in ["nginx", "nginx:1.27", "user/app", "docker.io/nginx", "ghcr.io/o/r:t", "localhost:5000/a"] {
            let once = normalize(image);
            assert_eq!(normalize(&once), once, "{image}");
        }
    }

    #[test]
    fn things_that_are_not_references_are_not_touched() {
        for junk in
            ["", "/nginx", "nginx/", "a//b", " nginx", "nginx ", "ng inx", "https://docker.io/nginx", "http://x/y"]
        {
            same(junk);
        }
    }
}
