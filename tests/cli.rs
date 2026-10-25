//! Drives the built binary against an in-process mock of the Sliplane API. Every test gets its
//! own config directory and the plaintext store, so nothing touches a real keystore or account.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

type Route = (&'static str, &'static str, u16, Value);

struct Mock {
    url: String,
    log: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    /// Routes are (method, path-with-query, status, body). Unmatched requests get a 404.
    fn start(routes: Vec<Route>) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v0/", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let routes = routes.clone();
                let log = log2.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").trim_start_matches("/v0/").to_string();
                    let mut headers = Vec::new();
                    let mut len = 0usize;
                    loop {
                        let mut h = String::new();
                        reader.read_line(&mut h).unwrap();
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            let (k, v) = (k.trim().to_lowercase(), v.trim().to_string());
                            if k == "content-length" {
                                len = v.parse().unwrap_or(0);
                            }
                            headers.push((k, v));
                        }
                    }
                    let mut buf = vec![0; len];
                    reader.read_exact(&mut buf).unwrap();
                    let body = (len > 0).then(|| serde_json::from_slice(&buf).unwrap());
                    log.lock().unwrap().push(Recorded { method: method.clone(), path: path.clone(), headers, body });

                    let (status, resp) = routes
                        .iter()
                        .find(|(m, p, _, _)| *m == method && *p == path)
                        .map(|(_, _, s, b)| (*s, b.clone()))
                        .unwrap_or((404, json!({"code": "not_found", "message": "no route"})));
                    let text = if status == 204 { String::new() } else { resp.to_string() };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    );
                });
            }
        });
        Mock { url, log }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }

    fn last(&self, method: &str) -> Recorded {
        self.requests().into_iter().rev().find(|r| r.method == method).expect("no such request")
    }
}

struct Env {
    dir: PathBuf,
    api: String,
}

impl Env {
    fn new(mock: &Mock) -> Env {
        // A counter, not the clock: macOS time has microsecond resolution, and two parallel tests
        // sharing a directory delete each other's config on drop.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sliplane-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Env { dir, api: mock.url.clone() }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sliplane"))
            .args(args)
            .env("SLIPLANE_CONFIG_DIR", &self.dir)
            .env("SLIPLANE_SECRET_STORE", "plaintext")
            .env("SLIPLANE_API_URL", &self.api)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> (i32, Value, Value) {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.run(&all);
        let parse = |b: &[u8]| {
            let s = String::from_utf8_lossy(b);
            // The plaintext store warns on stderr before any envelope.
            let s = s.lines().filter(|l| !l.starts_with("warning:")).collect::<Vec<_>>().join("\n");
            serde_json::from_str(&s).unwrap_or(Value::Null)
        };
        (out.status.code().unwrap(), parse(&out.stdout), parse(&out.stderr))
    }

    /// Adds account `work` with key `sl_test`, verified against the mock's `me`.
    fn with_account(self) -> Env {
        let (code, out, err) = self.json(&["accounts", "add", "work", "--api-key", "sl_test"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(out["status"], "added");
        self
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn me() -> Route {
    (
        "GET",
        "me",
        200,
        json!({"authType": "apikey", "tokenType": "rw", "organizationId": "org_1", "user": {"email": "dev@example.com"}}),
    )
}

const SVC: &str = "projects/p1/services/s1";

#[test]
fn accounts_lifecycle() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();

    assert_eq!(mock.last("GET").headers.iter().find(|(k, _)| k == "authorization").unwrap().1, "Bearer sl_test");

    let (code, out, _) = env.json(&["accounts", "list"]);
    assert_eq!(code, 0);
    assert_eq!(out["count"], 1);
    assert_eq!(out["accounts"][0]["identity"], "dev@example.com");
    assert_eq!(out["accounts"][0]["organization"], "org_1");
    assert_eq!(out["accounts"][0]["keyStatus"], "stored");
    assert_eq!(out["secretStore"], "plaintext");

    let (_, out, _) = env.json(&["accounts", "list", "--check"]);
    assert_eq!(out["accounts"][0]["keyStatus"], "valid");

    // Lookup is case-insensitive, and a duplicate needs --force.
    let (code, _, err) = env.json(&["accounts", "add", "WORK", "--api-key", "x"]);
    assert_eq!(code, 6);
    assert!(err["remediation"].as_str().unwrap().contains("--force"));

    let (code, out, _) = env.json(&["accounts", "test", "Work"]);
    assert_eq!(code, 0);
    assert_eq!(out["keyStatus"], "valid");

    // No terminal and no --yes: refuse rather than hang.
    let (code, _, err) = env.json(&["accounts", "remove", "work"]);
    assert_eq!(code, 6);
    assert_eq!(err["remediation"], "sliplane accounts remove work --yes");

    let (code, out, _) = env.json(&["accounts", "remove", "work", "--yes"]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "removed");
    let (_, out, _) = env.json(&["accounts", "list"]);
    assert_eq!(out["count"], 0);
}

#[test]
fn api_key_from_stdin() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sliplane"))
        .args(["accounts", "add", "piped", "--api-key-stdin", "--json"])
        .env("SLIPLANE_CONFIG_DIR", &env.dir)
        .env("SLIPLANE_SECRET_STORE", "plaintext")
        .env("SLIPLANE_API_URL", &env.api)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"sl_piped\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let auth = mock.last("GET").headers.into_iter().find(|(k, _)| k == "authorization").unwrap().1;
    assert_eq!(auth, "Bearer sl_piped");
}

#[test]
fn config_is_readable_yaml_without_secrets() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let yaml = std::fs::read_to_string(env.dir.join("config.yaml")).unwrap();
    assert!(yaml.contains("work:"), "{yaml}");
    assert!(yaml.contains("identity: dev@example.com"), "{yaml}");
    assert!(!yaml.contains("sl_test"), "the key leaked into config.yaml");
}

#[test]
fn account_is_required() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&["projects", "list"]);
    assert_eq!(code, 7);
    assert_eq!(err["code"], "no_account");
    assert!(err["detail"].as_str().unwrap().contains("work (dev@example.com)"));

    let (code, _, err) = env.json(&["projects", "list", "-a", "nope"]);
    assert_eq!(code, 7);
    assert_eq!(err["remediation"], "sliplane accounts list");
}

#[test]
fn http_errors_map_to_exit_codes() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects", 403, json!({"code": "read_only_token", "message": "Read-only"})),
        ("GET", "servers", 429, json!({"code": "rate_limit_exceeded", "message": "slow down"})),
        ("GET", "postgres", 500, json!({"code": "internal_server_error", "message": "boom"})),
    ]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&["projects", "create", "--name", "x", "-a", "work"]);
    assert_eq!(code, 3);
    assert!(err["error"].as_str().unwrap().contains("read-only"));
    assert!(err["detail"].as_str().unwrap().contains("HTTP 403"));

    assert_eq!(env.json(&["servers", "list", "-a", "work"]).0, 5);
    assert_eq!(env.json(&["postgres", "list", "-a", "work"]).0, 2);
    assert_eq!(env.json(&["servers", "get", "--server-id", "nope", "-a", "work"]).0, 4);
}

#[test]
fn parse_errors_are_envelopes() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let (code, _, err) = env.json(&["servers", "get", "-a", "work"]);
    assert_eq!(code, 6);
    assert_eq!(err["code"], "invalid_input");
    assert!(err["error"].as_str().unwrap().contains("--server-id"), "{err}");

    let out = env.run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn yaml_is_the_default() {
    let mock = Mock::start(vec![me(), ("GET", "projects", 200, json!([{"id": "p1", "name": "Main"}]))]);
    let env = Env::new(&mock).with_account();
    let out = env.run(&["projects", "list", "-a", "work"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("- id: p1"), "{stdout}");
    assert!(stdout.contains("name: Main"), "{stdout}");
}

#[test]
fn deletes_print_a_parseable_status() {
    let mock = Mock::start(vec![me(), ("DELETE", SVC, 204, json!(null))]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "delete", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, json!({"status": "deleted", "projectId": "p1", "serviceId": "s1"}));
}

#[test]
fn deploy_sends_an_empty_object() {
    let mock = Mock::start(vec![me(), ("POST", "projects/p1/services/s1/deploy", 202, json!(null))]);
    let env = Env::new(&mock).with_account();
    env.json(&["services", "deploy", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    let req = mock.last("POST");
    assert_eq!(req.body, Some(json!({})));
    assert!(req.headers.iter().any(|(k, v)| k == "content-type" && v == "application/json"));
}

#[test]
fn ids_are_escaped_in_paths() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    env.json(&["services", "delete-env", "--project-id", "p1", "--service-id", "s1", "--key", "A/B C", "-a", "work"]);
    assert_eq!(mock.last("DELETE").path, "projects/p1/services/s1/env/A%2FB%20C");
}

fn repo_service() -> Value {
    json!({
        "id": "s1",
        "deployment": {"url": "https://github.com/o/r", "branch": "develop", "dockerfilePath": "docker/Dockerfile",
                       "dockerContext": "app", "autoDeploy": false, "includePaths": ["src/**"]},
        "env": [{"key": "A", "value": "1", "secret": false},
                {"key": "B", "value": "2", "secret": false},
                {"key": "S", "value": "", "secret": true}]
    })
}

#[test]
fn update_carries_the_deployment_over() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--healthcheck",
        "/health",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    let body = mock.last("PATCH").body.unwrap();
    assert_eq!(body["healthcheck"], "/health");
    assert_eq!(
        body["deployment"],
        json!({"url": "https://github.com/o/r", "branch": "develop", "dockerfilePath": "docker/Dockerfile",
               "dockerContext": "app", "autoDeploy": false, "includePaths": ["src/**"]})
    );
    assert!(body.get("env").is_none());
}

#[test]
fn update_fails_when_a_branch_does_not_take() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["services", "update", "--project-id", "p1", "--service-id", "s1", "--branch", "main", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("branch 'develop' instead of 'main'"), "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap()["deployment"]["branch"], "main");
}

#[test]
fn update_refuses_to_drop_env() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["services", "update", "--project-id", "p1", "--service-id", "s1", "--env", "A=9", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("2 environment variable(s) not listed: B, S"), "{err}");
    assert!(mock.requests().iter().all(|r| r.method != "PATCH"));
}

#[test]
fn update_merges_env_and_keeps_secrets() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--env",
        "A=9",
        "--merge-env",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    let body = mock.last("PATCH").body.unwrap();
    assert_eq!(
        body["env"],
        json!([{"key": "B", "value": "2", "secret": false},
               {"key": "S", "value": "", "secret": true},
               {"key": "A", "value": "9", "secret": false}])
    );
    // The service is fetched once, however many things need it.
    assert_eq!(mock.requests().iter().filter(|r| r.method == "GET" && r.path == SVC).count(), 1);
}

#[test]
fn update_replace_env_sends_exactly_what_was_given() {
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, repo_service()), ("PATCH", SVC, 200, repo_service())]);
    let env = Env::new(&mock).with_account();
    let (code, _, _) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--env",
        "A=9",
        "--replace-env",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0);
    assert_eq!(mock.last("PATCH").body.unwrap()["env"], json!([{"key": "A", "value": "9", "secret": false}]));
}

#[test]
fn update_refuses_repo_flags_on_an_image_service() {
    let image = json!({"id": "s1", "deployment": {"url": "ghcr.io/o/app:1", "registryAuthenticationId": "cred_1"}});
    let mock = Mock::start(vec![me(), ("GET", SVC, 200, image.clone()), ("PATCH", SVC, 200, image)]);
    let env = Env::new(&mock).with_account();
    let (code, _, _) =
        env.json(&["services", "update", "--project-id", "p1", "--service-id", "s1", "--branch", "x", "-a", "work"]);
    assert_eq!(code, 6);

    // A new tag in the same registry keeps the credentials.
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--image",
        "ghcr.io/o/app:2",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("PATCH").body.unwrap()["deployment"],
        json!({"url": "ghcr.io/o/app:2", "registryAuthenticationId": "cred_1"})
    );
}

#[test]
fn update_refuses_windows_paths() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "update",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--healthcheck",
        "C:/Program Files/Git/",
        "-a",
        "work",
    ]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("Windows path"));
}

#[test]
fn create_requires_a_deployment_and_resolves_volume_names() {
    let mock = Mock::start(vec![
        me(),
        (
            "GET",
            "servers/srv1/volumes",
            200,
            json!([{"id": "volume_a", "name": "data"}, {"id": "volume_b", "name": "data"}]),
        ),
        ("POST", "projects/p1/services", 201, json!({"id": "s9"})),
    ]);
    let env = Env::new(&mock).with_account();
    let base = ["services", "create", "--project-id", "p1", "--name", "web", "--server-id", "srv1", "-a", "work"];

    let (code, _, err) = env.json(&base);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("--image"));

    let mut args = base.to_vec();
    args.extend([
        "--image",
        "nginx:latest",
        "--public",
        "--protocol",
        "http",
        "--env",
        "A=1",
        "--secret-env",
        "S=2",
        "--volume",
        "data:/data",
        "--volume",
        "fresh:/cache",
        "--volume",
        "volume_z:/z",
    ]);
    let (code, out, err) = env.json(&args);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s9");
    let body = mock.last("POST").body.unwrap();
    assert_eq!(body["deployment"], json!({"url": "nginx:latest"}));
    assert_eq!(body["network"], json!({"public": true, "protocol": "http"}));
    assert_eq!(
        body["volumes"],
        json!([{"id": "volume_a", "mountPath": "/data"}, {"name": "fresh", "mountPath": "/cache"},
               {"id": "volume_z", "mountPath": "/z"}])
    );
    assert_eq!(body["env"][1], json!({"key": "S", "value": "2", "secret": true}));

    let mut args = base.to_vec();
    args.extend(["--image", "nginx", "--protocol", "tcp"]);
    assert_eq!(env.json(&args).0, 6, "--protocol without --public");
}

#[test]
fn services_list_walks_every_project_in_order() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "projects", 200, json!([{"id": "p1"}, {"id": "p2"}, {"id": "p3"}])),
        ("GET", "projects/p1/services", 200, json!([{"id": "a"}])),
        ("GET", "projects/p2/services", 200, json!([])),
        ("GET", "projects/p3/services", 200, json!([{"id": "b"}, {"id": "c"}])),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, _) = env.json(&["services", "list", "-a", "work"]);
    assert_eq!(code, 0);
    assert_eq!(out, json!([{"id": "a"}, {"id": "b"}, {"id": "c"}]));
}

#[test]
fn postgres_and_bucket_bodies() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "postgres", 201, json!({"id": "pg1"})),
        ("PUT", "buckets/b1/cors", 200, json!({"rules": []})),
        ("GET", "postgres/pg1/logs?since=2026-09-01T00%3A00%3A00Z&limit=5", 200, json!([])),
        ("GET", "servers/srv1/metrics?range=1h", 200, json!([])),
    ]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&[
        "postgres",
        "create",
        "--name",
        "db",
        "--instance-type",
        "base",
        "--region",
        "ger",
        "--pg-version",
        "18",
        "--ip-allow",
        "203.0.113.0/24=office",
        "--ip-allow",
        "198.51.100.1/32",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("POST").body.unwrap(),
        json!({"name": "db", "instanceType": "base", "region": "ger", "version": 18,
               "ipAllowList": [{"source": "203.0.113.0/24", "description": "office"}, {"source": "198.51.100.1/32"}]})
    );

    let (code, _, err) = env.json(&[
        "buckets",
        "set-cors",
        "--bucket-id",
        "b1",
        "--allowed-origin",
        "*",
        "--allowed-method",
        "get",
        "--max-age",
        "60",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("PUT").body.unwrap(),
        json!({"rules": [{"allowedOrigins": ["*"], "allowedMethods": ["GET"], "maxAgeSeconds": 60}]})
    );

    let (code, _, err) = env.json(&[
        "postgres",
        "logs",
        "--postgres-id",
        "pg1",
        "--since",
        "2026-09-01T00:00:00Z",
        "--limit",
        "5",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(env.json(&["servers", "metrics", "--server-id", "srv1", "-a", "work"]).0, 0);
    assert_eq!(env.json(&["postgres", "logs", "--postgres-id", "pg1", "--limit", "5000", "-a", "work"]).0, 6);
}

#[test]
fn agent_readme_as_data() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    let (code, out, _) = env.json(&["agent-readme"]);
    assert_eq!(code, 0);
    assert_eq!(out["exitCodes"]["7"], "no_account - run sliplane accounts list");
    let md = String::from_utf8(env.run(&["agent-readme"]).stdout).unwrap();
    assert!(md.starts_with("# sliplane - agent operating manual"));
}

#[test]
fn login_command_lifecycle() {
    let mock = Mock::start(vec![me(), me(), me()]);
    let env = Env::new(&mock);

    // Login with default name "default"
    let (code, out, err) = env.json(&["login", "--api-key", "sl_login_test"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "logged_in");
    assert_eq!(out["name"], "default");
    assert_eq!(out["identity"], "dev@example.com");
    assert_eq!(out["organization"], "org_1");
    assert_eq!(out["secretStore"], "plaintext");

    // Re-login without --force fails
    let (code, _, err) = env.json(&["login", "--api-key", "sl_new_key"]);
    assert_eq!(code, 6);
    assert!(err["remediation"].as_str().unwrap().contains("--force"));

    // Re-login with --force succeeds
    let (code, out, _) = env.json(&["login", "--api-key", "sl_new_key", "--force"]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "logged_in");

    // Login with named account
    let (code, out, _) = env.json(&["login", "staging", "--api-key", "sl_staging"]);
    assert_eq!(code, 0);
    assert_eq!(out["name"], "staging");
}

// ---------------------------------------------------------------------------------------------
// API 0.10.0: scoped keys, SSH keys, service volumes, Postgres metrics, pricing

/// `me` as a key with explicit scopes answers it - what `GET /me` returns since API 0.10.0.
fn me_with_scopes(scopes: Value) -> Route {
    (
        "GET",
        "me",
        200,
        json!({"authType": "apikey", "tokenType": "rw", "organizationId": "org_1", "scopes": scopes,
               "user": {"email": "dev@example.com"}}),
    )
}

fn merge_env_args() -> Vec<&'static str> {
    vec!["services", "update", "--project-id", "p1", "--service-id", "s1", "--env", "A=9", "--merge-env", "-a", "work"]
}

#[test]
fn merge_env_refuses_when_the_key_cannot_read_secrets() {
    // Without secrets:read every value reads back empty, and a plain variable sent back empty is
    // overwritten with nothing. The secret S would have been safe; B would not.
    let blind = json!({"id": "s1", "deployment": repo_service()["deployment"].clone(),
        "env": [{"key": "A", "value": "", "secret": false}, {"key": "B", "value": "", "secret": false},
                {"key": "S", "value": "", "secret": true}]});
    let mock = Mock::start(vec![
        me_with_scopes(json!(["services:read", "services:write"])),
        ("GET", SVC, 200, blind),
        ("PATCH", SVC, 200, repo_service()),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&merge_env_args());
    assert_eq!(code, 3, "{err}");
    assert_eq!(err["code"], "auth_required");
    let message = err["error"].as_str().unwrap();
    assert!(message.contains("secrets:read") && message.contains("1 variable(s)") && message.contains(": B."), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("set-env"), "{err}");
    assert!(mock.requests().iter().all(|r| r.method != "PATCH"), "nothing may be sent");
}

#[test]
fn merge_env_without_the_secrets_scope_is_fine_when_only_secrets_are_carried() {
    // Secrets go back empty to mean "keep the stored value", which needs no read access.
    let only_secret = json!({"id": "s1", "deployment": repo_service()["deployment"].clone(),
        "env": [{"key": "A", "value": "", "secret": false}, {"key": "S", "value": "", "secret": true}]});
    let mock = Mock::start(vec![
        me_with_scopes(json!(["services:read", "services:write"])),
        ("GET", SVC, 200, only_secret),
        ("PATCH", SVC, 200, repo_service()),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&merge_env_args());
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("PATCH").body.unwrap()["env"],
        json!([{"key": "S", "value": "", "secret": true}, {"key": "A", "value": "9", "secret": false}])
    );
}

#[test]
fn merge_env_proceeds_with_the_secrets_scope() {
    let mock = Mock::start(vec![
        me_with_scopes(json!(["services:read", "services:write", "secrets:read"])),
        ("GET", SVC, 200, repo_service()),
        ("PATCH", SVC, 200, repo_service()),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&merge_env_args());
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap()["env"][0], json!({"key": "B", "value": "2", "secret": false}));
}

#[test]
fn accounts_warn_when_the_key_has_no_secrets_scope() {
    let mock = Mock::start(vec![me_with_scopes(json!(["services:read"]))]);
    let env = Env::new(&mock);
    let (code, out, err) = env.json(&["accounts", "add", "work", "--api-key", "sl_test"]);
    assert_eq!(code, 0, "{err}");
    assert!(out["warning"].as_str().unwrap().contains("secrets:read"), "{out}");
    let (_, out, _) = env.json(&["accounts", "test", "work"]);
    assert!(out["warning"].as_str().unwrap().contains("secrets:read"), "{out}");
    assert_eq!(out["me"]["scopes"], json!(["services:read"]));

    // A key that reports no scopes at all (legacy) and one that has the scope: no warning.
    let mock = Mock::start(vec![me()]);
    let (_, out, _) = Env::new(&mock).json(&["accounts", "add", "work", "--api-key", "sl_test"]);
    assert!(out.get("warning").is_none(), "{out}");
    let mock = Mock::start(vec![me_with_scopes(json!(["secrets:read"]))]);
    let (_, out, _) = Env::new(&mock).json(&["accounts", "add", "work", "--api-key", "sl_test"]);
    assert!(out.get("warning").is_none(), "{out}");
}

#[test]
fn ssh_key_commands() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "ssh-keys", 200, json!([{"id": "sshkey_1"}])),
        ("POST", "ssh-keys", 201, json!({"id": "sshkey_1", "fingerprint": "SHA256:x"})),
        ("DELETE", "ssh-keys/sshkey_1", 204, json!(null)),
        ("GET", "projects/p1/services/s1/ssh-keys", 200, json!([{"keyId": "sshkey_1"}])),
        ("PUT", "projects/p1/services/s1/ssh-keys/sshkey_1", 200, json!({"host": "h", "port": 2222, "username": "u"})),
        ("DELETE", "projects/p1/services/s1/ssh-keys/sshkey_1", 204, json!(null)),
    ]);
    let env = Env::new(&mock).with_account();

    let (code, out, _) = env.json(&["ssh-keys", "list", "-a", "work"]);
    assert_eq!((code, out), (0, json!([{"id": "sshkey_1"}])));

    let (code, _, err) = env.json(&[
        "ssh-keys",
        "create",
        "--name",
        "laptop",
        "--public-key",
        "ssh-ed25519 AAAAC3 me@laptop\n",
        "--expires-at",
        "2026-12-01T00:00:00Z",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        mock.last("POST").body.unwrap(),
        json!({"name": "laptop", "publicKey": "ssh-ed25519 AAAAC3 me@laptop", "expiresAt": "2026-12-01T00:00:00Z"})
    );

    // From a file, trimmed; no expiry key unless asked for.
    let file = env.dir.join("id.pub");
    std::fs::write(&file, "ssh-ed25519 BBBB me@desk\r\n").unwrap();
    let (code, _, err) =
        env.json(&["ssh-keys", "create", "--name", "desk", "--public-key-file", file.to_str().unwrap(), "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("POST").body.unwrap(), json!({"name": "desk", "publicKey": "ssh-ed25519 BBBB me@desk"}));

    let (code, out, _) = env.json(&["ssh-keys", "delete", "--key-id", "sshkey_1", "-a", "work"]);
    assert_eq!((code, out), (0, json!({"status": "deleted", "keyId": "sshkey_1"})));

    let svc = ["--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let (code, out, _) = env.json(&[&["services", "ssh-keys"][..], &svc].concat());
    assert_eq!((code, out), (0, json!([{"keyId": "sshkey_1"}])));

    let (code, out, err) = env.json(&[&["services", "attach-ssh-key", "--key-id", "sshkey_1"][..], &svc].concat());
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, json!({"host": "h", "port": 2222, "username": "u"}));
    // Attaching sends no body at all, as the spec declares none.
    assert_eq!(mock.last("PUT").body, None);

    let (code, out, _) = env.json(&[&["services", "detach-ssh-key", "--key-id", "sshkey_1"][..], &svc].concat());
    assert_eq!(
        (code, out),
        (0, json!({"status": "detached", "projectId": "p1", "serviceId": "s1", "keyId": "sshkey_1"}))
    );
}

#[test]
fn ssh_key_create_refuses_a_private_key_and_a_missing_key() {
    let mock = Mock::start(vec![me(), ("POST", "ssh-keys", 201, json!({}))]);
    let env = Env::new(&mock).with_account();

    let (code, _, err) = env.json(&[
        "ssh-keys",
        "create",
        "--name",
        "oops",
        "--public-key",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nabc",
        "-a",
        "work",
    ]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("private key"), "{err}");

    let (code, _, err) = env.json(&["ssh-keys", "create", "--name", "none", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("--public-key"), "{err}");

    let args = ["ssh-keys", "create", "--name", "x", "--public-key-file", "/no/such/file", "-a", "work"];
    assert_eq!(env.json(&args).0, 6);
    assert!(mock.requests().iter().all(|r| r.method != "POST"), "nothing may be sent");
}

#[test]
fn ssh_key_in_use_says_how_to_detach() {
    let mock = Mock::start(vec![
        me(),
        ("DELETE", "ssh-keys/sshkey_1", 409, json!({"code": "ssh_key_in_use", "message": "attached"})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["ssh-keys", "delete", "--key-id", "sshkey_1", "-a", "work"]);
    assert_eq!(code, 1);
    assert!(err["detail"].as_str().unwrap().contains("detach-ssh-key"), "{err}");
}

#[test]
fn service_volume_commands() {
    let volume = json!({"id": "volume_data", "name": "data", "mountPath": "/data"});
    let mock = Mock::start(vec![
        me(),
        ("GET", SVC, 200, json!({"id": "s1", "serverId": "srv1"})),
        (
            "GET",
            "servers/srv1/volumes",
            200,
            json!([{"id": "volume_data", "name": "data"}, {"id": "volume_other", "name": "other"}]),
        ),
        ("POST", "projects/p1/services/s1/volumes", 201, volume),
        (
            "PATCH",
            "projects/p1/services/s1/volumes/volume_data",
            200,
            json!({"id": "volume_data", "mountPath": "/mnt"}),
        ),
        ("DELETE", "projects/p1/services/s1/volumes/volume_data", 204, json!(null)),
        ("PATCH", "servers/srv1/volumes/volume_data", 200, json!({"id": "volume_data", "name": "renamed"})),
    ]);
    let env = Env::new(&mock).with_account();
    let svc = ["--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let run = |head: &[&str]| env.json(&[head, &svc].concat());

    // A name that matches an existing volume on the service's server mounts that one...
    let (code, _, err) = run(&["services", "attach-volume", "--volume", "data:/data"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("POST").body.unwrap(), json!({"id": "volume_data", "mountPath": "/data"}));

    // ...an unknown name creates one, and an id is used as given.
    run(&["services", "attach-volume", "--volume", "fresh:/fresh"]);
    assert_eq!(mock.last("POST").body.unwrap(), json!({"name": "fresh", "mountPath": "/fresh"}));
    run(&["services", "attach-volume", "--volume", "volume_other:/other"]);
    assert_eq!(mock.last("POST").body.unwrap(), json!({"id": "volume_other", "mountPath": "/other"}));

    let (code, _, err) = run(&["services", "attach-volume", "--volume", "data"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("id:/path or name:/path"), "{err}");

    let (code, _, err) = run(&["services", "update-volume", "--volume-id", "volume_data", "--mount-path", "/mnt"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap(), json!({"mountPath": "/mnt"}));

    // Git Bash on Windows turns "/mnt" into a drive path before the CLI sees it.
    let rewritten =
        ["services", "update-volume", "--volume-id", "volume_data", "--mount-path", "C:/Program Files/Git/mnt"];
    let (code, _, err) = run(&rewritten);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("Windows path"), "{err}");
    let relative = ["services", "update-volume", "--volume-id", "volume_data", "--mount-path", "mnt"];
    assert_eq!(run(&relative).0, 6);

    let (code, out, _) = run(&["services", "detach-volume", "--volume-id", "volume_data"]);
    assert_eq!(
        (code, out),
        (0, json!({"status": "detached", "projectId": "p1", "serviceId": "s1", "volumeId": "volume_data"}))
    );

    let (code, _, err) = env.json(&[
        "servers",
        "rename-volume",
        "--server-id",
        "srv1",
        "--volume-id",
        "volume_data",
        "--name",
        "renamed",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").body.unwrap(), json!({"name": "renamed"}));
}

#[test]
fn metrics_take_a_range_or_a_window_never_both() {
    let mock = Mock::start(vec![
        me(),
        ("GET", "servers/srv1/metrics?from=100&to=200", 200, json!([{"cpuUsage": 1}])),
        ("GET", "projects/p1/services/s1/metrics?range=7d", 200, json!([])),
        ("GET", "projects/p1/services/s1/metrics?range=1h", 200, json!([])),
        ("GET", "postgres/pg1/metrics?range=24h", 200, json!([{"activeConnections": 3}])),
        ("GET", "postgres/pg1/metrics?from=100&to=200", 200, json!([])),
    ]);
    let env = Env::new(&mock).with_account();
    let ok = |args: &[&str]| {
        let (code, out, err) = env.json(args);
        assert_eq!(code, 0, "{err}");
        out
    };

    // The API answers 400 to `range` together with from/to, so the 1h default must not ride along.
    ok(&["servers", "metrics", "--server-id", "srv1", "--from", "100", "--to", "200", "-a", "work"]);
    ok(&["services", "metrics", "--project-id", "p1", "--service-id", "s1", "--range", "7d", "-a", "work"]);
    ok(&["services", "metrics", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(
        ok(&["postgres", "metrics", "--postgres-id", "pg1", "--range", "24h", "-a", "work"]),
        json!([{"activeConnections": 3}])
    );
    ok(&["postgres", "metrics", "--postgres-id", "pg1", "--from", "100", "--to", "200", "-a", "work"]);

    // Contradictory or half-given windows are refused before any request.
    let before = mock.requests().len();
    let both =
        ["postgres", "metrics", "--postgres-id", "pg1", "--range", "1h", "--from", "1", "--to", "2", "-a", "work"];
    let from_only = ["postgres", "metrics", "--postgres-id", "pg1", "--from", "1", "-a", "work"];
    let to_only = ["servers", "metrics", "--server-id", "srv1", "--to", "2", "-a", "work"];
    for args in [both.as_slice(), from_only.as_slice(), to_only.as_slice()] {
        assert_eq!(env.json(args).0, 6, "{args:?}");
    }
    assert_eq!(mock.requests().len(), before);
}

#[test]
fn pricing_is_a_plain_get() {
    let mock = Mock::start(vec![me(), ("GET", "pricing", 200, json!({"currency": "EUR"}))]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["pricing", "-a", "work"]);
    assert_eq!((code, out), (0, json!({"currency": "EUR"})), "{err}");
}

#[test]
fn maintenance_mode_has_its_own_message() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects", 423, json!({"code": "organization_maintenance", "message": "locked"})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["projects", "create", "--name", "x", "-a", "work"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("maintenance"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("maintenance"), "{err}");
}
