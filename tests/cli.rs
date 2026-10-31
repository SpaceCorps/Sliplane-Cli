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

                    // How many times this route has been asked, this request included.
                    let nth = log.lock().unwrap().iter().filter(|r| r.method == method && r.path == path).count();
                    let (status, resp) = routes
                        .iter()
                        .find(|(m, p, _, _)| *m == method && *p == path)
                        .map(|(_, _, s, b)| (*s, b.clone()))
                        .unwrap_or((404, json!({"code": "not_found", "message": "no route"})));
                    // `{"__seq": [a, b, c]}` answers a, then b, then c for every call after.
                    let resp = match resp.get("__seq").and_then(Value::as_array) {
                        Some(seq) => seq[(nth - 1).min(seq.len() - 1)].clone(),
                        None => resp,
                    };
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
            .env("SLIPLANE_PRESETS_URL", format!("{}presets", self.api))
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> (i32, Value, Value) {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.run(&all);
        let parse = |b: &[u8]| {
            let s = String::from_utf8_lossy(b);
            // The plaintext store warns, and name lookups note what they chose, on stderr before any envelope.
            let s = s
                .lines()
                .filter(|l| !l.starts_with("warning:") && !l.starts_with("note:"))
                .collect::<Vec<_>>()
                .join("\n");
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

// ---------------------------------------------------------------------------------------------
// API 0.11.0: deploy hook rotation

const ROTATE: &str = "projects/p1/services/s1/deploy-hook/rotate";

#[test]
fn rotate_deploy_hook_sends_no_body_and_warns_that_the_old_url_is_dead() {
    let mock = Mock::start(vec![
        me(),
        ("POST", ROTATE, 200, json!({"deployHookUrl": "https://api.sliplane.io/deploy/s1/dhs_new"})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) =
        env.json(&["services", "rotate-deploy-hook", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["deployHookUrl"], "https://api.sliplane.io/deploy/s1/dhs_new");
    assert!(out["note"].as_str().unwrap().contains("stopped working immediately"), "{out}");
    assert_eq!(mock.last("POST").body, None, "the spec declares no request body");
}

#[test]
fn a_missing_scope_is_not_reported_as_a_generic_403() {
    let mock = Mock::start(vec![
        me(),
        (
            "POST",
            ROTATE,
            403,
            json!({"code": "insufficient_scope", "message": "This token is missing a required scope"}),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["services", "rotate-deploy-hook", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 3);
    assert_eq!(err["code"], "auth_required");
    assert!(err["error"].as_str().unwrap().contains("missing a permission"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("sliplane me"), "{err}");
    assert!(err["detail"].as_str().unwrap().contains("insufficient_scope"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// presets

fn catalog() -> Route {
    (
        "GET",
        "presets",
        200,
        json!({
            "generatedValues": [
                {"placeholder": "__SLIPLANE_GENERATED_0__", "kind": "string", "length": 4},
                {"placeholder": "__SLIPLANE_GENERATED_1__", "kind": "string", "length": 16}
            ],
            "presets": [
                {"name": "PostgreSQL", "tags": ["database"], "description": "A relational database.",
                 "volumes": [{"id": "pg-__SLIPLANE_GENERATED_0__", "name": "pg-data-__SLIPLANE_GENERATED_0__"}],
                 "settings": {"presetId": "postgres", "name": "Postgres-__SLIPLANE_GENERATED_0__",
                   "envs": [{"key": "POSTGRES_PASSWORD", "value": "__SLIPLANE_GENERATED_1__", "secret": false},
                            {"key": "POSTGRES_DB", "value": "mydb", "secret": false}],
                   "public": true, "protocol": "tcp", "deploySource": "registry",
                   "externalDockerImage": "docker.io/library/postgres:18.6", "cmd": "",
                   "volumes": [{"id": "pg-__SLIPLANE_GENERATED_0__", "containerMountPath": "/var/lib/postgresql"}]}},
                {"name": "n8n", "tags": ["automation"], "description": "Workflow automation.",
                 "volumes": [], "settings": {"presetId": "n8n", "name": "n8n-__SLIPLANE_GENERATED_0__",
                   "healthcheckPath": "/", "envs": [{"key": "OPENAI_API_KEY", "value": "", "secret": true}],
                   "public": true, "protocol": "http", "deploySource": "registry",
                   "externalDockerImage": "docker.io/n8nio/n8n:2.35.7", "cmd": "", "volumes": []}}
            ]
        }),
    )
}

#[test]
fn presets_list_and_show_need_no_account_and_never_send_a_key() {
    let mock = Mock::start(vec![catalog()]);
    let env = Env::new(&mock);

    let (code, out, err) = env.json(&["presets", "list"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.as_array().unwrap().len(), 2);
    assert_eq!(out[0]["id"], "postgres");
    assert_eq!(out[0]["image"], "docker.io/library/postgres:18.6");

    let (_, out, _) = env.json(&["presets", "list", "--search", "autom"]);
    assert_eq!(out.as_array().unwrap().len(), 1);
    assert_eq!(out[0]["id"], "n8n");
    let (_, out, _) = env.json(&["presets", "list", "--tag", "database"]);
    assert_eq!(out[0]["id"], "postgres");

    let (code, out, _) = env.json(&["presets", "show", "n8n"]);
    assert_eq!(code, 0);
    assert_eq!(out["env"][0]["key"], "OPENAI_API_KEY");
    assert_eq!(out["env"][0]["required"], true);

    let (code, _, err) = env.json(&["presets", "show", "nope"]);
    assert_eq!(code, 4);
    assert_eq!(err["remediation"], "sliplane presets list");

    for r in mock.requests() {
        assert!(!r.headers.iter().any(|(k, _)| k == "authorization"), "a key was sent to the catalog: {r:?}");
    }
}

#[test]
fn presets_deploy_needs_an_account_before_it_does_anything() {
    let mock = Mock::start(vec![me(), catalog()]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["presets", "deploy", "postgres", "--project-id", "p1", "--server-id", "srv1"]);
    assert_eq!(code, 7);
    assert_eq!(err["code"], "no_account");
    assert!(!mock.requests().iter().any(|r| r.path == "presets"), "the catalog was fetched without an account");
}

#[test]
fn presets_dry_run_creates_nothing_and_hides_secrets() {
    let mock = Mock::start(vec![me(), catalog()]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&[
        "presets",
        "deploy",
        "postgres",
        "--project-id",
        "p1",
        "--server-id",
        "srv1",
        "--dry-run",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["dryRun"], true);
    let password = out["request"]["env"].as_array().unwrap().iter().find(|e| e["key"] == "POSTGRES_PASSWORD").unwrap();
    assert_eq!(password["value"], "<hidden>");
    assert!(out["warnings"][0].as_str().unwrap().contains("--private"));
    assert!(!mock.requests().iter().any(|r| r.method == "POST"), "a dry run must not create anything");
}

#[test]
fn presets_deploy_creates_the_service_and_prints_the_generated_secrets_once() {
    let mock =
        Mock::start(vec![me(), catalog(), ("POST", "projects/p1/services", 201, json!({"id": "svc1", "name": "db"}))]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&[
        "presets",
        "deploy",
        "postgres",
        "--project-id",
        "p1",
        "--server-id",
        "srv1",
        "--name",
        "db",
        "--private",
        "--env",
        "POSTGRES_DB=orders",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");

    let body = mock.last("POST").body.unwrap();
    assert_eq!(body["name"], "db");
    assert_eq!(body["serverId"], "srv1");
    assert_eq!(body["deployment"], json!({"url": "docker.io/library/postgres:18.6"}));
    assert_eq!(body["network"], json!({"public": false}), "--private must not expose the database");
    let env_of = |k: &str| body["env"].as_array().unwrap().iter().find(|e| e["key"] == k).unwrap().clone();
    assert_eq!(env_of("POSTGRES_DB")["value"], "orders", "--env overrides the preset's value");
    assert_eq!(body["volumes"][0]["mountPath"], "/var/lib/postgresql");
    assert!(body["volumes"][0]["name"].as_str().unwrap().starts_with("pg-data-"));

    // The password that was sent is the one printed, because it can never be read back later.
    assert_eq!(out["service"]["id"], "svc1");
    assert_eq!(out["generated"][0]["key"], "POSTGRES_PASSWORD");
    assert_eq!(out["generated"][0]["value"], env_of("POSTGRES_PASSWORD")["value"]);
    assert_eq!(out["generated"][0]["value"].as_str().unwrap().len(), 16);
    assert!(out["warnings"].is_null(), "private, so nothing to warn about: {out}");
}

#[test]
fn presets_deploy_refuses_an_empty_required_value_without_creating_anything() {
    let mock = Mock::start(vec![me(), catalog()]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) =
        env.json(&["presets", "deploy", "n8n", "--project-id", "p1", "--server-id", "srv1", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["remediation"].as_str().unwrap().contains("--secret-env OPENAI_API_KEY=<value>"));
    assert!(!mock.requests().iter().any(|r| r.method == "POST"));
}

fn event(id: &str, kind: &str, message: &str) -> Value {
    json!({"id": id, "type": kind, "message": message, "createdAt": "2026-10-31T12:00:00Z"})
}

#[test]
fn presets_deploy_wait_reports_the_outcome() {
    let mock = Mock::start(vec![
        me(),
        catalog(),
        ("POST", "projects/p1/services", 201, json!({"id": "svc1"})),
        (
            "GET",
            "projects/p1/services/svc1/events",
            200,
            json!([event("e1", "service_deploy_success", "Deploy succeeded")]),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let args = ["presets", "deploy", "postgres", "--project-id", "p1", "--server-id", "srv1", "--wait", "-a", "work"];
    let (code, out, err) = env.json(&args);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["deploy"]["status"], "live");
    assert_eq!(out["deploy"]["event"]["id"], "e1");
}

#[test]
fn a_failed_wait_still_prints_the_secrets_it_generated() {
    let mock = Mock::start(vec![
        me(),
        catalog(),
        ("POST", "projects/p1/services", 201, json!({"id": "svc1"})),
        (
            "GET",
            "projects/p1/services/svc1/events",
            200,
            json!([event("e1", "service_deploy_failed", "image pull failed")]),
        ),
    ]);
    let env = Env::new(&mock).with_account();
    let args = ["presets", "deploy", "postgres", "--project-id", "p1", "--server-id", "srv1", "--wait", "-a", "work"];
    let (code, out, err) = env.json(&args);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("image pull failed"), "{err}");
    // The service exists, and its password is unrecoverable unless it was printed.
    assert_eq!(out["service"]["id"], "svc1");
    assert_eq!(out["generated"][0]["value"].as_str().unwrap().len(), 16);
    assert_eq!(out["deploy"]["status"], "not_confirmed");
}

// ---------------------------------------------------------------------------------------------
// --wait on services

const EVENTS: &str = "projects/p1/services/s1/events";

fn deploy_wait(env: &Env, extra: &[&str]) -> (i32, Value, Value) {
    let mut args = vec!["services", "deploy", "--project-id", "p1", "--service-id", "s1", "--wait", "-a", "work"];
    args.extend_from_slice(extra);
    env.json(&args)
}

#[test]
fn deploy_wait_ignores_old_events_and_reports_the_new_outcome() {
    let old = event("e0", "service_deploy_success", "an earlier deploy");
    let new = event("e1", "service_deploy_success", "this deploy");
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!({"__seq": [[old.clone()], [old, new]]})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = deploy_wait(&env, &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "live");
    assert_eq!(out["action"], "deploy");
    assert_eq!(out["event"]["id"], "e1", "the earlier deploy's success must not count");

    // The baseline is read before the deploy is triggered, or the new events would be ignored too.
    let order: Vec<String> = mock.requests().iter().map(|r| format!("{} {}", r.method, r.path)).collect();
    let post = order.iter().position(|r| r.ends_with("/deploy")).unwrap();
    assert!(order[..post].iter().any(|r| r.ends_with("/events")), "{order:?}");
}

#[test]
fn deploy_wait_polls_until_there_is_an_outcome() {
    let old = event("e0", "service_deploy_success", "an earlier deploy");
    let new = event("e1", "service_deploy_success", "this deploy");
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!({"__seq": [[old.clone()], [old.clone()], [old, new]]})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = deploy_wait(&env, &[]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["event"]["id"], "e1");
    assert!(mock.requests().iter().filter(|r| r.path == EVENTS).count() >= 3);
}

#[test]
fn deploy_wait_fails_when_the_deploy_does() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!({"__seq": [[], [event("e1", "service_build_failed", "Dockerfile not found")]]})),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = deploy_wait(&env, &[]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("Dockerfile not found"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("services logs"));
}

#[test]
fn deploy_wait_gives_up_at_the_timeout() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services/s1/deploy", 202, json!(null)),
        ("GET", EVENTS, 200, json!([event("e0", "service_deploy_success", "old")])),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = deploy_wait(&env, &["--timeout", "1"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("No deploy outcome after 1 seconds"), "{err}");
}

#[test]
fn timeout_without_wait_is_refused() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let (code, _, _) =
        env.json(&["services", "deploy", "--project-id", "p1", "--service-id", "s1", "--timeout", "5", "-a", "work"]);
    assert_eq!(code, 6);
}

#[test]
fn create_wait_reports_the_deploy_on_the_service() {
    let mock = Mock::start(vec![
        me(),
        ("POST", "projects/p1/services", 201, json!({"id": "s9", "name": "web"})),
        ("GET", "projects/p1/services/s9/events", 200, json!([event("e1", "service_deploy_success", "ok")])),
    ]);
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&[
        "services",
        "create",
        "--project-id",
        "p1",
        "--name",
        "web",
        "--server-id",
        "srv1",
        "--image",
        "nginx",
        "--wait",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s9");
    assert_eq!(out["deploy"]["status"], "live");
}

// ---------------------------------------------------------------------------------------------
// logs

const LOGS: &str = "projects/p1/services/s1/logs";
const LOGS_FROM_EPOCH_2000: &str = "projects/p1/services/s1/logs?from=951868800";
const LOGS_FROM_LATEST: &str = "projects/p1/services/s1/logs?from=951868801";

fn log_line(at: &str, message: &str) -> Value {
    json!({"createdAt": at, "message": message})
}

#[test]
fn logs_since_becomes_from() {
    let mock = Mock::start(vec![me(), ("GET", LOGS_FROM_EPOCH_2000, 200, json!([]))]);
    let env = Env::new(&mock).with_account();
    let base = ["services", "logs", "--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let with = |extra: &[&str]| {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        env.json(&args)
    };

    let (code, _, err) = with(&["--since", "2000-03-01T00:00:00Z"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("GET").path, LOGS_FROM_EPOCH_2000);

    let (code, _, err) = with(&["--since", "yesterday"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("neither a duration"));
}

#[test]
fn logs_follow_prints_each_line_once_oldest_first() {
    let (a, b, c) = (
        log_line("2000-03-01T00:00:01Z", "a"),
        log_line("2000-03-01T00:00:02Z", "b"),
        log_line("2000-03-01T00:00:03Z", "c"),
    );
    let mock = Mock::start(vec![
        me(),
        // Newest first, as the API sends them.
        ("GET", LOGS, 200, json!([b.clone(), a])),
        // The next poll starts one second before the newest line seen, so "b" comes back.
        ("GET", LOGS_FROM_LATEST, 200, json!([c, b])),
    ]);
    let env = Env::new(&mock).with_account();
    let out = env.run(&[
        "services",
        "logs",
        "--project-id",
        "p1",
        "--service-id",
        "s1",
        "--follow",
        "--interval",
        "1",
        "--for",
        "1s",
        "--json",
        "-a",
        "work",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with("warning:"))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let messages: Vec<&str> = lines.iter().map(|l| l["message"].as_str().unwrap()).collect();
    assert_eq!(messages, ["a", "b", "c"]);

    let polls: Vec<String> =
        mock.requests().iter().filter(|r| r.path.starts_with(LOGS)).map(|r| r.path.clone()).collect();
    assert_eq!(polls[0], LOGS);
    assert_eq!(polls[1], LOGS_FROM_LATEST);
}

#[test]
fn logs_follow_flags_are_checked() {
    let mock = Mock::start(vec![me()]);
    let env = Env::new(&mock).with_account();
    let base = ["services", "logs", "--project-id", "p1", "--service-id", "s1", "-a", "work"];
    let with = |extra: &[&str]| {
        let mut args = base.to_vec();
        args.extend_from_slice(extra);
        env.json(&args)
    };
    assert_eq!(with(&["--follow", "--to", "5"]).0, 6, "--to pages backwards; following goes forwards");
    assert_eq!(with(&["--interval", "3"]).0, 6, "--interval needs --follow");
    assert_eq!(with(&["--for", "1m"]).0, 6, "--for needs --follow");
    assert_eq!(with(&["--since", "5m", "--from", "100"]).0, 6);
    let (code, _, err) = with(&["--follow", "--for", "5x"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("not a duration"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// version, completion

#[test]
fn version_reports_the_build_and_the_api_it_targets() {
    let mock = Mock::start(vec![]);
    let (code, out, _) = Env::new(&mock).json(&["version"]);
    assert_eq!(code, 0);
    assert_eq!(out["name"], "sliplane");
    assert_eq!(out["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(out["apiSpec"], "0.11.0");
}

#[test]
fn completion_prints_a_script_for_each_shell() {
    let mock = Mock::start(vec![]);
    let env = Env::new(&mock);
    for (shell, marker) in [
        ("bash", "complete -F"),
        ("zsh", "#compdef sliplane"),
        ("fish", "complete -c sliplane"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        let out = env.run(&["completion", shell]);
        assert_eq!(out.status.code(), Some(0), "{shell}");
        let script = String::from_utf8_lossy(&out.stdout);
        assert!(script.contains(marker), "{shell}: {}", &script[..script.len().min(200)]);
        assert!(script.contains("presets"), "{shell} completions should know the new commands");
    }
    assert_eq!(env.run(&["completion", "tcsh"]).status.code(), Some(6));
}

// ---------------------------------------------------------------------------------------------
// names for ids

fn two_projects() -> Vec<Route> {
    vec![
        ("GET", "projects", 200, json!([{"id": "p1", "name": "Main"}, {"id": "p2", "name": "Lab"}])),
        (
            "GET",
            "projects/p1/services",
            200,
            json!([{"id": "s1", "name": "web", "projectId": "p1"}, {"id": "s2", "name": "worker", "projectId": "p1"}]),
        ),
        ("GET", "projects/p2/services", 200, json!([{"id": "s3", "name": "web", "projectId": "p2"}])),
        ("GET", "projects/p1/services/s1", 200, json!({"id": "s1", "name": "web"})),
        ("GET", "projects/p1/services/s2", 200, json!({"id": "s2", "name": "worker"})),
        ("GET", "projects/p2/services/s3", 200, json!({"id": "s3", "name": "web"})),
    ]
}

fn mock_with(extra: Vec<Route>) -> Mock {
    let mut routes = vec![me()];
    routes.extend(extra);
    Mock::start(routes)
}

fn paths(mock: &Mock, method: &str) -> Vec<String> {
    mock.requests().into_iter().filter(|r| r.method == method).map(|r| r.path).collect()
}

#[test]
fn ids_alone_make_no_lookup_requests() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let before = mock.requests().len();
    let (code, _, err) = env.json(&["services", "get", "--project-id", "p1", "--service-id", "s1", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    let made: Vec<String> = mock.requests()[before..].iter().map(|r| r.path.clone()).collect();
    assert_eq!(made, ["projects/p1/services/s1"], "an id-only command must cost exactly one request");
}

#[test]
fn a_service_is_found_by_name_within_a_named_project() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "get", "--project", "Main", "--service", "web", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s1");
    assert_eq!(mock.last("GET").path, "projects/p1/services/s1");
    let gets = paths(&mock, "GET");
    assert!(gets.contains(&"projects".to_string()) && gets.contains(&"projects/p1/services".to_string()), "{gets:?}");
}

#[test]
fn a_unique_name_is_found_without_naming_the_project() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "get", "--service", "worker", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s2");
    assert_eq!(mock.last("GET").path, "projects/p1/services/s2");
}

#[test]
fn an_id_alone_finds_its_project() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "get", "--service-id", "s3", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["id"], "s3");
    assert_eq!(mock.last("GET").path, "projects/p2/services/s3");

    let (code, _, err) = env.json(&["services", "get", "--service-id", "nope", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["error"].as_str().unwrap().contains("'nope'"), "{err}");
}

#[test]
fn a_name_two_services_share_is_refused_with_both_ids() {
    let mut routes = two_projects();
    routes.push(("DELETE", "projects/p2/services/s3", 204, json!(null)));
    let mock = mock_with(routes);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["services", "delete", "--service", "web", "-a", "work"]);
    assert_eq!(code, 6, "{err}");
    let detail = err["detail"].as_str().unwrap();
    assert!(detail.contains("s1 (project p1)") && detail.contains("s3 (project p2)"), "{detail}");
    assert!(err["remediation"].as_str().unwrap().contains("--project"), "{err}");
    assert!(paths(&mock, "DELETE").is_empty(), "nothing may be deleted when the name is ambiguous");

    // Naming the project makes it unambiguous.
    let (code, _, err) = env.json(&["services", "delete", "--project", "Lab", "--service", "web", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("DELETE").path, "projects/p2/services/s3");
}

#[test]
fn an_unknown_name_is_not_found_and_shows_what_exists() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["services", "get", "--project", "Main", "--service", "nope", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["detail"].as_str().unwrap().contains("worker (s2)"), "{err}");
    assert_eq!(err["remediation"], "sliplane services list --account <account>");

    let (code, _, err) = env.json(&["services", "get", "--project", "Nowhere", "--service", "web", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["error"].as_str().unwrap().contains("No project"), "{err}");
}

#[test]
fn an_id_and_a_name_for_the_same_thing_conflict_and_neither_is_refused_early() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock);
    // No account exists here: both must fail at argument checking, before any keystore or network use.
    let (code, _, err) = env.json(&["services", "get", "--service-id", "s1", "--service", "web", "-a", "work"]);
    assert_eq!(code, 6, "{err}");
    let (code, _, err) = env.json(&["services", "get", "--project-id", "p1", "-a", "work"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("--service-id"), "{err}");
    assert!(mock.requests().is_empty());
}

#[test]
fn servers_databases_and_buckets_take_names() {
    let mock = mock_with(vec![
        ("GET", "servers", 200, json!([{"id": "server_1", "name": "big"}])),
        ("GET", "servers/server_1", 200, json!({"id": "server_1"})),
        ("GET", "postgres", 200, json!([{"id": "pg_1", "name": "orders"}])),
        ("GET", "postgres/pg_1", 200, json!({"id": "pg_1"})),
        ("GET", "buckets", 200, json!([{"id": "bucket_1", "name": "assets"}])),
        ("GET", "buckets/bucket_1/keys", 200, json!([])),
    ]);
    let env = Env::new(&mock).with_account();
    assert_eq!(env.json(&["servers", "get", "--server", "big", "-a", "work"]).0, 0);
    assert_eq!(mock.last("GET").path, "servers/server_1");
    assert_eq!(env.json(&["postgres", "get", "--postgres", "orders", "-a", "work"]).0, 0);
    assert_eq!(mock.last("GET").path, "postgres/pg_1");
    assert_eq!(env.json(&["buckets", "keys", "--bucket", "assets", "-a", "work"]).0, 0);
    assert_eq!(mock.last("GET").path, "buckets/bucket_1/keys");
    let (code, _, err) = env.json(&["postgres", "get", "--postgres", "nope", "-a", "work"]);
    assert_eq!(code, 4);
    assert!(err["error"].as_str().unwrap().contains("Postgres database"), "{err}");
}

#[test]
fn projects_are_renamed_and_deleted_by_name() {
    let mock = mock_with(
        two_projects()
            .into_iter()
            .take(1)
            .chain([
                ("PATCH", "projects/p1", 200, json!({"id": "p1", "name": "Primary"})),
                ("DELETE", "projects/p2", 204, json!(null)),
            ])
            .collect(),
    );
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&["projects", "update", "--project", "Main", "--name", "Primary", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(mock.last("PATCH").path, "projects/p1");
    let (code, out, err) = env.json(&["projects", "delete", "--project", "Lab", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["projectId"], "p2");
    assert_eq!(mock.last("DELETE").path, "projects/p2");
}

#[test]
fn create_takes_a_project_and_server_by_name() {
    let mut routes = two_projects().into_iter().take(1).collect::<Vec<_>>();
    routes.push(("GET", "servers", 200, json!([{"id": "server_1", "name": "big"}])));
    routes.push(("POST", "projects/p1/services", 201, json!({"id": "s9"})));
    let mock = mock_with(routes);
    let env = Env::new(&mock).with_account();
    let (code, _, err) = env.json(&[
        "services",
        "create",
        "--project",
        "Main",
        "--name",
        "api",
        "--server",
        "big",
        "--image",
        "nginx",
        "-a",
        "work",
    ]);
    assert_eq!(code, 0, "{err}");
    let body = mock.last("POST").body.unwrap();
    assert_eq!(body["serverId"], "server_1");
    assert_eq!(mock.last("POST").path, "projects/p1/services");
}

#[test]
fn services_list_takes_a_project_by_name() {
    let mock = mock_with(two_projects());
    let env = Env::new(&mock).with_account();
    let (code, out, err) = env.json(&["services", "list", "--project", "Lab", "-a", "work"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out[0]["id"], "s3");
    assert_eq!(mock.last("GET").path, "projects/p2/services");
}

// ---------------------------------------------------------------------------------------------
// s3: files in a bucket, over the S3 protocol
//
// Two servers are involved. `Mock` plays the Sliplane API (bucket list, key create/delete) and
// `MockS3` plays the bucket's endpoint. The mock S3 checks every request's Signature V4 itself,
// with code written separately from the CLI's, and its payload hash against the body it received,
// so a request that real S3 would refuse is refused here too.
//
// The same scenario also runs against a real S3-compatible server when one is named:
//   SLIPLANE_TEST_S3_ENDPOINT=http://127.0.0.1:19000 SLIPLANE_TEST_S3_BUCKET=testbucket \
//   SLIPLANE_TEST_S3_ACCESS_KEY=minioadmin SLIPLANE_TEST_S3_SECRET_KEY=minioadmin cargo test s3_

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const S3_ACCESS: &str = "AKIAMOCKEXAMPLE";
const S3_SECRET: &str = "mockSecretKey/ForTests+Only0123456789";

#[derive(Clone, Debug)]
struct S3Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

struct S3Opts {
    bucket: &'static str,
    /// The first this-many requests are refused as if the key were not active yet.
    reject_first: usize,
    /// Answer every request with a redirect to this URL instead.
    redirect: Option<String>,
}

impl Default for S3Opts {
    fn default() -> S3Opts {
        S3Opts { bucket: "files", reject_first: 0, redirect: None }
    }
}

struct MockS3 {
    url: String,
    seen: Arc<Mutex<Vec<S3Seen>>>,
}

impl MockS3 {
    fn requests(&self) -> Vec<S3Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// What a server does with a Signature V4 request: rebuild the canonical request from what arrived
/// on the wire and compare signatures. Nothing is shared with the CLI's signer.
fn signature_ok(method: &str, path: &str, query: &str, headers: &[(String, String)], secret: &str) -> bool {
    let Some(auth) = header_of(headers, "authorization") else { return false };
    let field = |name: &str| auth.split(name).nth(1).map(|r| r.split([',', ' ']).next().unwrap_or("")).unwrap_or("");
    let credential = field("Credential=");
    let signed = field("SignedHeaders=");
    let given = field("Signature=");
    let scope: Vec<&str> = credential.split('/').collect();
    if scope.len() != 5 || scope[2] != "us-east-1" || scope[3] != "s3" || scope[4] != "aws4_request" {
        return false;
    }
    let mut pairs: Vec<&str> = if query.is_empty() { Vec::new() } else { query.split('&').collect() };
    pairs.sort();
    let canonical_headers: String =
        signed.split(';').map(|h| format!("{h}:{}\n", header_of(headers, h).unwrap_or("").trim())).collect();
    let payload = header_of(headers, "x-amz-content-sha256").unwrap_or("");
    let canonical = format!("{method}\n{path}\n{}\n{canonical_headers}\n{signed}\n{payload}", pairs.join("&"));
    let amz_date = header_of(headers, "x-amz-date").unwrap_or("");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{}/{}/s3/aws4_request\n{}",
        scope[1],
        scope[2],
        hex_of(&Sha256::digest(canonical.as_bytes()))
    );
    let mut key = hmac_sha256(format!("AWS4{secret}").as_bytes(), scope[1].as_bytes());
    for part in [scope[2], "s3", "aws4_request"] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    hex_of(&hmac_sha256(&key, string_to_sign.as_bytes())) == given
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// How AWS writes a key when asked for `encoding-type=url`: spaces as `+`, `/` kept.
fn list_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn s3_error(status: u16, code: &str, message: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{message}</Message><RequestId>REQ1</RequestId></Error>"
    );
    (status, vec![("Content-Type".into(), "application/xml".into())], body.into_bytes())
}

/// ListObjectsV2 over an in-memory bucket, the way S3 does it: entries (keys and folded prefixes)
/// in order, a page of at most `max-keys`, and the last entry returned as the continuation token.
fn list_response(objects: &BTreeMap<String, Vec<u8>>, query: &str, bucket: &str) -> Vec<u8> {
    let q: BTreeMap<String, String> =
        query.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (pct_decode(k), pct_decode(v))).collect();
    let prefix = q.get("prefix").cloned().unwrap_or_default();
    let delimiter = q.get("delimiter").cloned().unwrap_or_default();
    let max: usize = q.get("max-keys").and_then(|m| m.parse().ok()).unwrap_or(1000);
    let after = q.get("continuation-token").cloned();
    let url = q.get("encoding-type").is_some_and(|e| e == "url");
    let enc = |s: &str| if url { list_encode(s) } else { s.to_string() };

    // (name, is_folder), in S3's order.
    let mut entries: Vec<(String, bool)> = Vec::new();
    for key in objects.keys().filter(|k| k.starts_with(&prefix)) {
        let rest = &key[prefix.len()..];
        match (!delimiter.is_empty()).then(|| rest.find(&delimiter)).flatten() {
            Some(i) => {
                let folder = format!("{prefix}{}", &rest[..i + delimiter.len()]);
                if entries.last().map(|(n, f)| (n.as_str(), *f)) != Some((folder.as_str(), true)) {
                    entries.push((folder, true));
                }
            }
            None => entries.push((key.clone(), false)),
        }
    }
    if let Some(after) = &after {
        entries.retain(|(n, _)| n > after);
    }
    let truncated = entries.len() > max;
    entries.truncate(max);

    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>{bucket}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{max}</MaxKeys>",
        xml_escape(&enc(&prefix)),
        entries.len()
    );
    if url {
        xml.push_str("<EncodingType>url</EncodingType>");
    }
    xml.push_str(&format!("<IsTruncated>{truncated}</IsTruncated>"));
    if truncated {
        xml.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            xml_escape(&entries.last().unwrap().0)
        ));
    }
    for (name, folder) in &entries {
        if *folder {
            xml.push_str(&format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", xml_escape(&enc(name))));
        } else {
            let body = &objects[name];
            xml.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>2026-10-31T12:00:00.000Z</LastModified><ETag>&quot;{}&quot;</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                xml_escape(&enc(name)),
                &hex_of(&Sha256::digest(body))[..32],
                body.len()
            ));
        }
    }
    xml.push_str("</ListBucketResult>");
    xml.into_bytes()
}

fn start_s3(opts: S3Opts) -> MockS3 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let objects = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
    let counter = Arc::new(AtomicUsize::new(0));
    let opts = Arc::new(opts);
    let log = seen.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let (log, objects, counter, opts) = (log.clone(), objects.clone(), counter.clone(), opts.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let target = parts.next().unwrap_or("").to_string();
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 {
                        return;
                    }
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.push((k.trim().to_lowercase(), v.trim().to_string()));
                    }
                }
                let len: usize = header_of(&headers, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                let mut body = vec![0; len];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                log.lock().unwrap().push(S3Seen {
                    method: method.clone(),
                    target: target.clone(),
                    headers: headers.clone(),
                });

                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let (status, mut out_headers, out_body) = (|| {
                    if let Some(to) = &opts.redirect {
                        return (307, vec![("Location".to_string(), format!("{to}{target}"))], Vec::new());
                    }
                    let credential = header_of(&headers, "authorization").unwrap_or("");
                    if !credential.contains(&format!("Credential={S3_ACCESS}/")) {
                        return s3_error(
                            403,
                            "InvalidAccessKeyId",
                            "The Access Key Id you provided does not exist in our records.",
                        );
                    }
                    if counter.fetch_add(1, Ordering::SeqCst) < opts.reject_first {
                        return s3_error(403, "InvalidAccessKeyId", "The key is not active yet.");
                    }
                    if !signature_ok(&method, path, query, &headers, S3_SECRET) {
                        return s3_error(
                            403,
                            "SignatureDoesNotMatch",
                            "The request signature we calculated does not match.",
                        );
                    }
                    // The payload hash that was signed has to be the hash of the body that came.
                    let claimed = header_of(&headers, "x-amz-content-sha256").unwrap_or("");
                    if claimed != "UNSIGNED-PAYLOAD" && claimed != hex_of(&Sha256::digest(&body)) {
                        return s3_error(
                            400,
                            "XAmzContentSHA256Mismatch",
                            "The provided x-amz-content-sha256 header does not match what was computed.",
                        );
                    }
                    let Some(rest) = path.strip_prefix('/').and_then(|p| p.strip_prefix(opts.bucket)) else {
                        return s3_error(404, "NoSuchBucket", "The specified bucket does not exist");
                    };
                    let key = pct_decode(rest.strip_prefix('/').unwrap_or(""));
                    let mut store = objects.lock().unwrap();
                    match (method.as_str(), key.is_empty()) {
                        ("GET", true) => (
                            200,
                            vec![("Content-Type".into(), "application/xml".into())],
                            list_response(&store, query, opts.bucket),
                        ),
                        ("PUT", false) => {
                            let etag = format!("\"{}\"", &hex_of(&Sha256::digest(&body))[..32]);
                            store.insert(key, body.clone());
                            (200, vec![("ETag".into(), etag)], Vec::new())
                        }
                        ("GET" | "HEAD", false) => match store.get(&key) {
                            Some(b) => (
                                200,
                                vec![("Content-Length".into(), b.len().to_string())],
                                if method == "GET" { b.clone() } else { Vec::new() },
                            ),
                            None if method == "HEAD" => (404, Vec::new(), Vec::new()),
                            None => s3_error(404, "NoSuchKey", "The specified key does not exist."),
                        },
                        ("DELETE", false) => {
                            store.remove(&key);
                            (204, Vec::new(), Vec::new())
                        }
                        _ => s3_error(400, "InvalidRequest", "not something this mock does"),
                    }
                })();

                // A HEAD answers with the length of the object it did not send.
                let declared =
                    out_headers.iter().position(|(k, _)| k == "Content-Length").map(|i| out_headers.remove(i).1);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Length: {}\r\n",
                    declared.unwrap_or(out_body.len().to_string())
                );
                for (k, v) in &out_headers {
                    let _ = write!(stream, "{k}: {v}\r\n");
                }
                let _ = write!(stream, "\r\n");
                let _ = stream.write_all(&out_body);
            });
        }
    });
    MockS3 { url, seen }
}

/// A Sliplane API that knows one bucket, `sb_1`, and hands out S3 keys for it.
fn api_for(endpoint: &str, bucket: &str, access: &str, secret: &str, status: &str) -> Mock {
    Mock::start(vec![
        me(),
        (
            "GET",
            "buckets",
            200,
            json!([
                {"id": "sb_other", "name": "elsewhere", "endpoint": "https://other.example", "status": "active"},
                {"id": "sb_1", "name": bucket, "region": "ger", "endpoint": endpoint, "status": status}
            ]),
        ),
        (
            "POST",
            "buckets/sb_1/keys",
            201,
            json!({"id": "sk_1", "name": "sliplane-cli (work)", "accessKeyId": access, "secretAccessKey": secret, "createdAt": "2026-10-31T12:00:00Z"}),
        ),
        ("DELETE", "buckets/sb_1/keys/sk_1", 204, json!({})),
    ])
}

fn run_with(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sliplane"))
        .args(args)
        .env("SLIPLANE_CONFIG_DIR", &env.dir)
        .env("SLIPLANE_SECRET_STORE", "plaintext")
        .env("SLIPLANE_API_URL", &env.api)
        .envs(extra.iter().copied())
        .output()
        .unwrap()
}

fn json_with(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> (i32, Value, Value) {
    let mut all = args.to_vec();
    all.push("--json");
    let out = run_with(env, &all, extra);
    let parse = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        let s =
            s.lines().filter(|l| !l.starts_with("warning:") && !l.starts_with("note:")).collect::<Vec<_>>().join("\n");
        serde_json::from_str(&s).unwrap_or(Value::Null)
    };
    (out.status.code().unwrap(), parse(&out.stdout), parse(&out.stderr))
}

/// Deterministic bytes that do not compress, `len` of them.
fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x9E3779B97F4A7C15u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn sha_hex(bytes: &[u8]) -> String {
    hex_of(&Sha256::digest(bytes))
}

/// Where the scenario's S3 server is, and the key it accepts.
struct S3Target {
    endpoint: String,
    bucket: String,
    access: String,
    secret: String,
}

fn keys_of(v: &Value) -> Vec<String> {
    v["objects"].as_array().unwrap().iter().map(|o| o["key"].as_str().unwrap().to_string()).collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
}

/// Everything a user does with a bucket, in order, against whichever server `t` names.
fn files_scenario(t: &S3Target) {
    let api = api_for(&t.endpoint, &t.bucket, &t.access, &t.secret, "active");
    let env = Env::new(&api).with_account();
    let run = |args: &[&str]| -> (i32, Value, Value) {
        let mut all = vec!["s3"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["-a", "work", "--bucket", &t.bucket]);
        env.json(&all)
    };
    // Real servers keep what earlier runs left; everything here lives under a folder of its own.
    let p = format!("sl-test-{}-{}", std::process::id(), env.dir.file_name().unwrap().to_string_lossy());

    // 1. Without a key, nothing happens, and the way forward is spelled out.
    let (code, _, err) = run(&["ls"]);
    assert_eq!(code, 3, "{err}");
    assert!(err["remediation"].as_str().unwrap().starts_with("sliplane s3 connect -a work --bucket-id sb_1"), "{err}");
    assert!(api.requests().iter().all(|r| r.method != "POST"), "ls made a key");

    // 2. connect makes one, keeps it, and prints nothing secret.
    let (code, out, err) = run(&["connect"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "connected");
    assert_eq!(out["reused"], false);
    assert_eq!(out["bucket"], t.bucket.as_str());
    assert_eq!(out["keyId"], "sk_1");
    assert_eq!(out["accessKeyId"], t.access.as_str());
    assert_eq!(out["secretStore"], "plaintext");
    let raw = env.run(&["s3", "connect", "-a", "work", "--bucket", &t.bucket]);
    let shown = format!("{}{}", String::from_utf8_lossy(&raw.stdout), String::from_utf8_lossy(&raw.stderr));
    assert!(!shown.contains(&t.secret), "the secret was printed: {shown}");
    let post = api.requests().into_iter().filter(|r| r.method == "POST").collect::<Vec<_>>();
    assert_eq!(post.len(), 1, "the second connect made another key");
    assert_eq!(post[0].path, "buckets/sb_1/keys");
    assert_eq!(post[0].body.as_ref().unwrap()["name"], "sliplane-cli (work)");
    // The pair is in the keystore (the plaintext one, here) and nowhere else.
    let secrets_file = std::fs::read_to_string(env.dir.join("secrets.json")).unwrap();
    assert!(secrets_file.contains("s3:work:sb_1") && secrets_file.contains(&t.secret));
    let config = std::fs::read_to_string(env.dir.join("config.yaml")).unwrap();
    assert!(!config.contains(&t.secret) && !config.contains(&t.access), "{config}");
    let (_, again, _) = run(&["connect"]);
    assert_eq!(again["reused"], true);

    // 3. Uploads: a small file under an awkward key, a refusal to overwrite, and a big one.
    let small = env.dir.join("small.txt");
    std::fs::write(&small, b"hello, bucket\n").unwrap();
    let small = small.to_str().unwrap();
    let awkward = format!("{p}/docs/hello world+1.txt");
    let (code, out, err) = run(&["upload", "--file", small, "--key", &awkward]);
    assert_eq!(code, 0, "{err}");
    assert_eq!((out["status"].as_str(), out["size"].as_u64()), (Some("uploaded"), Some(14)));
    assert_eq!(out["key"], awkward.as_str());
    assert!(!out["etag"].as_str().unwrap().is_empty());
    let (code, _, err) = run(&["upload", "--file", small, "--key", &awkward]);
    assert_eq!(code, 6, "{err}");
    assert!(err["error"].as_str().unwrap().contains("already exists"));
    assert!(err["remediation"].as_str().unwrap().contains("--force"));
    assert_eq!(run(&["upload", "--file", small, "--key", &awkward, "--force"]).0, 0);

    let unicode = format!("{p}/docs/caf\u{e9}/\u{fc}n\u{ef}.txt");
    assert_eq!(run(&["upload", "--file", small, "--key", &unicode]).0, 0);
    assert_eq!(run(&["upload", "--file", small, "--key", &format!("{p}/top.txt")]).0, 0);

    let big_bytes = noise(20 * 1024 * 1024 + 3);
    let big = env.dir.join("blob.bin");
    std::fs::write(&big, &big_bytes).unwrap();
    let big_key = format!("{p}/big/blob.bin");
    let (code, out, err) = run(&["upload", "--file", big.to_str().unwrap(), "--key", &big_key]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["size"], big_bytes.len());
    // With no --key the object is named after the file.
    let named_file = env.dir.join(format!("{p}-named.txt"));
    std::fs::write(
        &named_file,
        b"named
",
    )
    .unwrap();
    let named_key = format!("{p}-named.txt");
    let (code, out, err) = run(&["upload", "--file", named_file.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["key"], named_key.as_str());

    // 4. Listings: folded at '/', below a prefix, and all the way down.
    let (code, out, err) = run(&["ls", "--prefix", &format!("{p}/")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(keys_of(&out), [format!("{p}/top.txt")]);
    assert_eq!(strings(&out["prefixes"]), [format!("{p}/big/"), format!("{p}/docs/")]);
    assert_eq!(out["truncated"], false);
    assert_eq!(out["bucket"], t.bucket.as_str());

    let (_, out, _) = run(&["ls", "--prefix", &format!("{p}/docs/")]);
    assert_eq!(keys_of(&out), [awkward.clone()]);
    assert_eq!(strings(&out["prefixes"]), [format!("{p}/docs/caf\u{e9}/")]);
    let o = &out["objects"][0];
    assert_eq!(o["size"], 14);
    assert!(o["lastModified"].as_str().unwrap().starts_with("20"), "{o}");
    assert!(!o["etag"].as_str().unwrap().is_empty() && !o["etag"].as_str().unwrap().contains('"'));

    let (_, out, _) = run(&["ls", "--recursive", "--prefix", &format!("{p}/")]);
    assert_eq!(out["count"], 4);
    assert!(strings(&out["prefixes"]).is_empty());
    let all = keys_of(&out);
    assert!(all.contains(&unicode) && all.contains(&awkward) && all.contains(&big_key), "{all:?}");
    assert_eq!(
        out["objects"].as_array().unwrap().iter().find(|o| o["key"] == big_key.as_str()).unwrap()["size"],
        big_bytes.len()
    );

    // ...and in pages. One entry at a time, every page is followed; with a limit, it stops and says how to go on.
    let page = [("SLIPLANE_S3_PAGE_SIZE", "2")];
    let prefix = format!("{p}/");
    let (code, out, err) = json_with(
        &env,
        &["s3", "ls", "-a", "work", "--bucket", &t.bucket, "--recursive", "--limit", "0", "--prefix", &prefix],
        &page,
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["count"], 4);
    assert_eq!(out["truncated"], false);
    let (_, first, _) = json_with(
        &env,
        &["s3", "ls", "-a", "work", "--bucket", &t.bucket, "--recursive", "--limit", "3", "--prefix", &prefix],
        &page,
    );
    assert_eq!((first["count"].as_u64(), first["truncated"].as_bool()), (Some(3), Some(true)));
    let token = first["nextToken"].as_str().unwrap().to_string();
    let (_, rest, _) = json_with(
        &env,
        &["s3", "ls", "-a", "work", "--bucket", &t.bucket, "--recursive", "--prefix", &prefix, "--token", &token],
        &page,
    );
    assert_eq!(rest["truncated"], false);
    let mut joined = keys_of(&first);
    joined.extend(keys_of(&rest));
    assert_eq!(joined, all, "pages did not add up to the listing");

    // 5. Downloads, byte for byte.
    let small_out = env.dir.join("got-small.txt");
    let (code, out, err) = run(&["download", "--key", &awkward, "--out", small_out.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!((out["status"].as_str(), out["size"].as_u64()), (Some("downloaded"), Some(14)));
    assert_eq!(std::fs::read(&small_out).unwrap(), b"hello, bucket\n");
    let big_out = env.dir.join("got-blob.bin");
    let (code, out, err) = run(&["download", "--key", &big_key, "--out", big_out.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["size"], big_bytes.len());
    let got = std::fs::read(&big_out).unwrap();
    assert_eq!(sha_hex(&got), sha_hex(&big_bytes), "the download is not the upload");
    // Nothing is left half-written beside it.
    let leftovers: Vec<_> = std::fs::read_dir(&env.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // An existing file is not replaced unless asked; an existing directory gets the key's file name.
    let (code, _, err) = run(&["download", "--key", &awkward, "--out", small_out.to_str().unwrap()]);
    assert_eq!(code, 6, "{err}");
    assert_eq!(std::fs::read(&small_out).unwrap(), b"hello, bucket\n");
    std::fs::write(&small_out, b"old").unwrap();
    assert_eq!(run(&["download", "--key", &awkward, "--out", small_out.to_str().unwrap(), "--force"]).0, 0);
    assert_eq!(std::fs::read(&small_out).unwrap(), b"hello, bucket\n");
    let dir = env.dir.join("into");
    std::fs::create_dir(&dir).unwrap();
    assert_eq!(run(&["download", "--key", &unicode, "--out", dir.to_str().unwrap()]).0, 0);
    assert!(dir.join("\u{fc}n\u{ef}.txt").is_file());

    // To stdout the object is the output, and the receipt goes to stderr.
    let raw =
        env.run(&["s3", "download", "-a", "work", "--bucket", &t.bucket, "--key", &big_key, "--out", "-", "--json"]);
    assert_eq!(raw.status.code(), Some(0));
    assert_eq!(sha_hex(&raw.stdout), sha_hex(&big_bytes));
    let stderr = String::from_utf8_lossy(&raw.stderr);
    let text =
        stderr.lines().filter(|l| !l.starts_with("warning:") && !l.starts_with("note:")).collect::<Vec<_>>().join(
            "
",
        );
    let receipt: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text:?}"));
    assert_eq!(receipt["path"], "-");

    // A key that is not there is not_found, whichever way it is asked for.
    let (code, _, err) =
        run(&["download", "--key", &format!("{p}/nope.txt"), "--out", env.dir.join("nope").to_str().unwrap()]);
    assert_eq!((code, err["code"].as_str()), (4, Some("not_found")), "{err}");
    assert!(
        !env.dir.join("nope").exists()
            && std::fs::read_dir(&env.dir).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".part"))
    );

    // 6. Deleting: all or nothing, and a typo is an error rather than a success.
    let (code, _, err) = run(&["rm", "--key", &format!("{p}/top.txt"), "--key", &format!("{p}/typo.txt")]);
    assert_eq!(code, 4, "{err}");
    assert!(err["detail"].as_str().unwrap().contains("typo.txt"));
    assert_eq!(
        keys_of(&run(&["ls", "--prefix", &format!("{p}/top")]).1),
        [format!("{p}/top.txt")],
        "something was deleted"
    );
    let (code, out, err) =
        run(&["rm", "--key", &format!("{p}/top.txt"), "--key", &named_key, "--key", &format!("{p}/top.txt")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!((out["status"].as_str(), out["count"].as_u64()), (Some("deleted"), Some(2)));
    for k in [&awkward, &unicode, &big_key] {
        assert_eq!(run(&["rm", "--key", k]).0, 0);
    }
    assert_eq!(run(&["ls", "--recursive", "--prefix", &format!("{p}/")]).1["count"], 0);

    // 7. Forgetting the key: locally only, and it says how to revoke it.
    let (code, out, err) = run(&["disconnect"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!((out["status"].as_str(), out["keyId"].as_str()), (Some("disconnected"), Some("sk_1")));
    assert!(out["note"].as_str().unwrap().contains("buckets delete-key -a work --bucket-id sb_1 --key-id sk_1"));
    assert!(api.requests().iter().all(|r| r.method != "DELETE"), "disconnect revoked the key");
    assert!(!std::fs::read_to_string(env.dir.join("secrets.json")).unwrap().contains(&t.secret));
    assert_eq!(run(&["ls"]).0, 3);
    assert_eq!(run(&["disconnect"]).1["status"], "not_connected");

    // The API only ever saw the API key, and never an S3 header.
    for r in api.requests() {
        let auth = r.headers.iter().find(|(k, _)| k == "authorization").map(|(_, v)| v.as_str());
        assert_eq!(auth, Some("Bearer sl_test"), "{} {}", r.method, r.path);
        assert!(r.headers.iter().all(|(_, v)| !v.contains(&t.access) && !v.contains(&t.secret)));
    }
}

#[test]
fn s3_files_against_the_mock() {
    let s3 = start_s3(S3Opts::default());
    files_scenario(&S3Target {
        endpoint: s3.url.clone(),
        bucket: "files".into(),
        access: S3_ACCESS.into(),
        secret: S3_SECRET.into(),
    });

    let seen = s3.requests();
    assert!(seen.len() > 30, "{}", seen.len());
    for r in &seen {
        // Path-style, signed, and nothing of the API's.
        assert!(r.target.starts_with("/files"), "{}", r.target);
        let auth = header_of(&r.headers, "authorization").unwrap();
        assert!(auth.starts_with(&format!("AWS4-HMAC-SHA256 Credential={S3_ACCESS}/")), "{auth}");
        assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"), "{auth}");
        assert!(r.headers.iter().all(|(_, v)| !v.contains("sl_test")), "the API key reached the S3 endpoint: {r:?}");
        assert!(!r.headers.iter().any(|(k, _)| k == "x-organization-id"));
    }
    // A file is hashed before it is sent, so the signed payload is a real digest (never unsigned).
    let puts: Vec<_> = seen.iter().filter(|r| r.method == "PUT").collect();
    assert!(!puts.is_empty());
    for r in puts {
        let h = header_of(&r.headers, "x-amz-content-sha256").unwrap();
        assert!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()), "{h}");
        assert!(header_of(&r.headers, "content-length").is_some());
    }
    // Keys with spaces, plus signs and non-ASCII travel percent-encoded exactly once.
    assert!(
        seen.iter().any(|r| r.target.contains("hello%20world%2B1.txt")),
        "{:?}",
        seen.iter().map(|r| &r.target).collect::<Vec<_>>()
    );
    assert!(seen.iter().any(|r| r.target.contains("caf%C3%A9/%C3%BCn%C3%AF.txt")));
}

#[test]
fn s3_files_against_a_real_server_when_one_is_named() {
    let Ok(endpoint) = std::env::var("SLIPLANE_TEST_S3_ENDPOINT") else {
        eprintln!("skipped: SLIPLANE_TEST_S3_ENDPOINT is not set (see the notes above the s3 tests)");
        return;
    };
    let var = |k: &str, default: &str| std::env::var(k).unwrap_or_else(|_| default.to_string());
    files_scenario(&S3Target {
        endpoint,
        bucket: var("SLIPLANE_TEST_S3_BUCKET", "testbucket"),
        access: var("SLIPLANE_TEST_S3_ACCESS_KEY", "minioadmin"),
        secret: var("SLIPLANE_TEST_S3_SECRET_KEY", "minioadmin"),
    });
}

/// More than one page of real keys. Slow (it uploads 1,205), so it only runs when asked for twice.
#[test]
fn s3_a_real_server_lists_more_than_one_page() {
    let (Ok(endpoint), Ok(_)) = (std::env::var("SLIPLANE_TEST_S3_ENDPOINT"), std::env::var("SLIPLANE_TEST_S3_MANY"))
    else {
        eprintln!("skipped: needs SLIPLANE_TEST_S3_ENDPOINT and SLIPLANE_TEST_S3_MANY=1");
        return;
    };
    let var = |k: &str, default: &str| std::env::var(k).unwrap_or_else(|_| default.to_string());
    let (bucket, access, secret) = (
        var("SLIPLANE_TEST_S3_BUCKET", "testbucket"),
        var("SLIPLANE_TEST_S3_ACCESS_KEY", "minioadmin"),
        var("SLIPLANE_TEST_S3_SECRET_KEY", "minioadmin"),
    );
    let api = api_for(&endpoint, &bucket, &access, &secret, "active");
    let env = Env::new(&api).with_account();
    assert_eq!(env.json(&["s3", "connect", "-a", "work", "--bucket", &bucket]).0, 0);
    let p = format!("many-{}", std::process::id());
    let file = env.dir.join("one.txt");
    std::fs::write(&file, b"x").unwrap();
    let total = 1205;

    // Eight uploaders at a time; every one is a separate run of the CLI.
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let (env, bucket, p, file) = (&env, &bucket, &p, &file);
            scope.spawn(move || {
                for i in (worker..total).step_by(8) {
                    let key = format!("{p}/k{i:05}");
                    let out = run_with(
                        env,
                        &[
                            "s3",
                            "upload",
                            "-a",
                            "work",
                            "--bucket",
                            bucket,
                            "--file",
                            file.to_str().unwrap(),
                            "--key",
                            &key,
                        ],
                        &[],
                    );
                    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
                }
            });
        }
    });

    let prefix = format!("{p}/");
    let (code, out, err) =
        env.json(&["s3", "ls", "-a", "work", "--bucket", &bucket, "--recursive", "--limit", "0", "--prefix", &prefix]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["count"], total, "the listing stopped at the first page");
    let keys = keys_of(&out);
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "keys repeat or are out of order");
    // The default limit (1000) stops with a token, and the token finishes the job.
    let (_, first, _) = env.json(&["s3", "ls", "-a", "work", "--bucket", &bucket, "--recursive", "--prefix", &prefix]);
    assert_eq!((first["count"].as_u64(), first["truncated"].as_bool()), (Some(1000), Some(true)));
    let token = first["nextToken"].as_str().unwrap();
    let (_, rest, _) = env.json(&[
        "s3",
        "ls",
        "-a",
        "work",
        "--bucket",
        &bucket,
        "--recursive",
        "--prefix",
        &prefix,
        "--token",
        token,
    ]);
    assert_eq!(rest["count"], total - 1000);
    assert_eq!(rest["truncated"], false);

    let mut args = vec!["s3", "rm", "-a", "work", "--bucket", &bucket];
    let doomed: Vec<String> = keys.iter().take(1).cloned().collect();
    for k in &doomed {
        args.extend(["--key", k]);
    }
    assert_eq!(env.json(&args).0, 0);
    for chunk in keys.chunks(8).skip(1) {
        let mut args = vec!["s3", "rm", "-a", "work", "--bucket", &bucket];
        for k in chunk {
            args.extend(["--key", k]);
        }
        assert_eq!(env.json(&args).0, 0);
    }
    assert_eq!(
        env.json(&["s3", "ls", "-a", "work", "--bucket", &bucket, "--recursive", "--prefix", &prefix]).1["count"],
        0
    );
}

#[test]
fn s3_a_key_that_is_not_active_yet_is_waited_for() {
    let s3 = start_s3(S3Opts { reject_first: 2, ..Default::default() });
    let api = api_for(&s3.url, "files", S3_ACCESS, S3_SECRET, "active");
    let env = Env::new(&api).with_account();
    let started = std::time::Instant::now();
    let (code, out, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "connected");
    assert!(started.elapsed() >= std::time::Duration::from_secs(2), "it did not wait");
    assert_eq!(s3.requests().len(), 3, "two refusals, then the one that worked");
}

#[test]
fn s3_a_key_that_never_activates_is_kept_and_the_retry_reuses_it() {
    let s3 = start_s3(S3Opts { reject_first: 3, ..Default::default() });
    let api = api_for(&s3.url, "files", S3_ACCESS, S3_SECRET, "active");
    let env = Env::new(&api).with_account();
    let (code, _, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files", "--timeout", "2"]);
    assert_eq!(code, 1, "{err}");
    assert!(err["error"].as_str().unwrap().contains("did not become usable within 2s"), "{err}");
    assert!(err["detail"].as_str().unwrap().contains("InvalidAccessKeyId"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("sliplane s3 connect -a work --bucket-id sb_1"));
    // The key was made and kept; the retry makes no second one.
    assert!(std::fs::read_to_string(env.dir.join("secrets.json")).unwrap().contains("s3:work:sb_1"));
    let (code, out, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files", "--timeout", "10"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["reused"], true);
    assert_eq!(api.requests().iter().filter(|r| r.method == "POST").count(), 1);
}

#[test]
fn s3_a_wrong_secret_is_an_auth_error_that_keeps_the_s3_code() {
    let s3 = start_s3(S3Opts::default());
    let api = api_for(&s3.url, "files", S3_ACCESS, "not-the-secret", "active");
    let env = Env::new(&api).with_account();
    let (code, _, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files"]);
    assert_eq!(code, 3, "{err}");
    assert_eq!(err["code"], "auth_required");
    assert!(err["detail"].as_str().unwrap().contains("SignatureDoesNotMatch"), "{err}");
    assert!(err["detail"].as_str().unwrap().contains("HTTP 403"));
    assert!(err["remediation"].as_str().unwrap().contains("s3 disconnect"));
    assert!(!err.to_string().contains("not-the-secret"));
}

#[test]
fn s3_a_redirect_is_not_followed_and_nothing_is_sent_after_it() {
    let target = start_s3(S3Opts::default());
    let s3 = start_s3(S3Opts { redirect: Some(target.url.clone()), ..Default::default() });
    let api = api_for(&s3.url, "files", S3_ACCESS, S3_SECRET, "active");
    let env = Env::new(&api).with_account();
    let (code, _, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files"]);
    assert_eq!(code, 1, "{err}");
    assert!(err["error"].as_str().unwrap().contains("redirect"), "{err}");
    assert!(err["detail"].as_str().unwrap().contains("HTTP 307"));
    assert_eq!(s3.requests().len(), 1);
    assert!(target.requests().is_empty(), "the redirect was followed: {:?}", target.requests());
}

#[test]
fn s3_an_endpoint_that_would_send_keys_in_the_clear_costs_nothing() {
    // Plain HTTP to anything but loopback would send signed requests, and file contents, unencrypted.
    let api = api_for("http://s3.example.com", "files", S3_ACCESS, S3_SECRET, "active");
    let env = Env::new(&api).with_account();
    let (code, _, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files"]);
    assert_eq!(code, 1, "{err}");
    assert!(err["error"].as_str().unwrap().contains("plain HTTP"), "{err}");
    assert!(api.requests().iter().all(|r| r.method != "POST"), "a key was made for an endpoint that cannot be used");
}

#[test]
fn s3_a_bucket_that_is_going_away_is_refused() {
    let s3 = start_s3(S3Opts::default());
    let api = api_for(&s3.url, "files", S3_ACCESS, S3_SECRET, "queued_for_deletion");
    let env = Env::new(&api).with_account();
    let (code, _, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files"]);
    assert_eq!(code, 6, "{err}");
    assert!(err["error"].as_str().unwrap().contains("queued_for_deletion"));
    assert!(api.requests().iter().all(|r| r.method != "POST"));
}

#[test]
fn s3_arguments_are_checked_before_any_request() {
    let api = api_for("http://127.0.0.1:9", "files", S3_ACCESS, S3_SECRET, "active");
    let env = Env::new(&api).with_account();
    let before = api.requests().len();
    let run = |args: &[&str]| {
        let mut all = vec!["s3"];
        all.extend_from_slice(args);
        env.json(&all)
    };
    // No bucket named.
    assert_eq!(run(&["ls", "-a", "work"]).0, 6);
    // No account named.
    assert_eq!(run(&["ls", "--bucket", "files"]).0, 7);
    // A key Git Bash mangled from "/logs/a.txt".
    let (code, _, err) = run(&["rm", "-a", "work", "--bucket", "files", "--key", "C:/Program Files/Git/logs/a.txt"]);
    assert_eq!(code, 6);
    assert!(err["error"].as_str().unwrap().contains("Windows path"));
    let (code, _, _) = run(&["ls", "-a", "work", "--bucket", "files", "--prefix", "C:/Program Files/Git/logs"]);
    assert_eq!(code, 6);
    // Empty and over-long keys; a file that is not there; a directory.
    assert_eq!(run(&["rm", "-a", "work", "--bucket", "files", "--key", ""]).0, 6);
    assert_eq!(run(&["rm", "-a", "work", "--bucket", "files", "--key", &"k".repeat(1025)]).0, 6);
    assert_eq!(run(&["upload", "-a", "work", "--bucket", "files", "--file", "no-such-file.bin"]).0, 6);
    assert_eq!(run(&["upload", "-a", "work", "--bucket", "files", "--file", env.dir.to_str().unwrap()]).0, 6);
    assert_eq!(run(&["download", "-a", "work", "--bucket", "files", "--key", "logs/"]).0, 6);
    assert_eq!(api.requests().len(), before, "an invalid call still made a request");
}

#[test]
fn s3_disconnect_by_id_needs_no_network() {
    let api = api_for("http://127.0.0.1:9", "files", S3_ACCESS, S3_SECRET, "active");
    let env = Env::new(&api).with_account();
    let before = api.requests().len();
    let (code, out, err) = env.json(&["s3", "disconnect", "-a", "work", "--bucket-id", "sb_gone"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "not_connected");
    assert_eq!(api.requests().len(), before);
}

#[test]
fn s3_connect_revokes_the_key_it_cannot_keep() {
    let s3 = start_s3(S3Opts::default());
    let api = api_for(&s3.url, "files", S3_ACCESS, S3_SECRET, "active");
    let env = Env::new(&api).with_account();
    // A keystore that cannot be written: the file it writes through is a directory.
    std::fs::create_dir(env.dir.join("secrets.json.tmp")).unwrap();
    let (code, _, err) = env.json(&["s3", "connect", "-a", "work", "--bucket", "files"]);
    assert_ne!(code, 0, "{err}");
    assert!(err["error"].as_str().unwrap().contains("could not be stored"), "{err}");
    assert!(err["detail"].as_str().unwrap().contains("sk_1 was revoked"), "{err}");
    assert_eq!(api.last("DELETE").path, "buckets/sb_1/keys/sk_1");
    assert!(s3.requests().is_empty(), "the unusable key was probed");
}
