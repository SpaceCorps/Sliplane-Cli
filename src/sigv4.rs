//! AWS Signature Version 4, header-based, as the S3 protocol uses it. No presigned URLs and no
//! chunked-payload signing: the payload hash is either the SHA-256 of the body or one the caller
//! computed beforehand.
//!
//! S3 differs from the other AWS services in one way that matters here: the path is encoded once
//! and never normalised, so a key is signed exactly as it travels. [`sign`] therefore takes the
//! canonical URI already encoded (see [`uri_encode`]) and does not touch it.
//!
//! Checked against the AWS Signature V4 test suite and the worked examples in the S3 documentation;
//! the vectors are in the tests below.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

/// `Hex(SHA256(""))` - the payload hash of every request without a body.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const ALGORITHM: &str = "AWS4-HMAC-SHA256";

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Percent-encodes everything but the unreserved characters (`A-Z a-z 0-9 - _ . ~`), as UTF-8 bytes
/// in upper-case hex. `keep_slash` leaves `/` alone, for an object key in a path; in a query string
/// it is encoded.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// What gets signed. Header names may be in any case, and values are normalised (trimmed, runs of
/// whitespace collapsed) the way the canonical form wants. `headers` must hold `host`, and for S3
/// also `x-amz-content-sha256` and `x-amz-date`.
pub struct Request<'a> {
    pub method: &'a str,
    /// Already percent-encoded, starting with `/`.
    pub canonical_uri: &'a str,
    /// Raw, unencoded pairs, in any order.
    pub query: &'a [(&'a str, &'a str)],
    pub headers: &'a [(&'a str, &'a str)],
    /// Hex SHA-256 of the body, or `UNSIGNED-PAYLOAD`.
    pub payload_hash: &'a str,
}

pub struct Scope<'a> {
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub region: &'a str,
    pub service: &'a str,
    /// `YYYYMMDDTHHMMSSZ`
    pub amz_date: &'a str,
}

/// The intermediate values are kept, though only `authorization` is sent: they are what the
/// test vectors compare, and what a `SignatureDoesNotMatch` is debugged with.
#[allow(dead_code)]
pub struct Signature {
    pub canonical_request: String,
    pub string_to_sign: String,
    pub signature: String,
    pub signed_headers: String,
    /// The value of the `Authorization` header.
    pub authorization: String,
}

pub fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> =
        query.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

pub fn sign(req: &Request, scope: &Scope) -> Signature {
    let mut headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        req.canonical_uri,
        canonical_query(req.query),
        canonical_headers,
        signed_headers,
        req.payload_hash
    );

    let date = &scope.amz_date[..8];
    let credential_scope = format!("{date}/{}/{}/aws4_request", scope.region, scope.service);
    let string_to_sign =
        format!("{ALGORITHM}\n{}\n{credential_scope}\n{}", scope.amz_date, sha256_hex(canonical_request.as_bytes()));

    let k_date = hmac(format!("AWS4{}", scope.secret_key).as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, scope.region.as_bytes());
    let k_service = hmac(&k_region, scope.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "{ALGORITHM} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        scope.access_key
    );
    Signature { canonical_request, string_to_sign, signature, signed_headers, authorization }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The credentials and clock of the AWS Signature V4 test suite.
    const SUITE_KEY: &str = "AKIDEXAMPLE";
    const SUITE_SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const SUITE_DATE: &str = "20150830T123600Z";

    /// Signs a request of the AWS test suite (region us-east-1, service `service`) and returns the
    /// signature. Their `.authz` files carry the expected values; the suite is published at
    /// docs.aws.amazon.com/general/latest/gr/signature-v4-test-suite.html and the signatures below
    /// were checked against the copy in the AWS SDK for JavaScript's fixtures.
    fn suite(method: &str, uri: &str, query: &[(&str, &str)], extra: &[(&str, &str)], payload: &str) -> Signature {
        let mut headers = vec![("Host", "example.amazonaws.com"), ("X-Amz-Date", SUITE_DATE)];
        headers.extend_from_slice(extra);
        sign(
            &Request { method, canonical_uri: uri, query, headers: &headers, payload_hash: payload },
            &Scope {
                access_key: SUITE_KEY,
                secret_key: SUITE_SECRET,
                region: "us-east-1",
                service: "service",
                amz_date: SUITE_DATE,
            },
        )
    }

    fn authz(signed_headers: &str, signature: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders={signed_headers}, Signature={signature}"
        )
    }

    #[test]
    fn suite_get_vanilla_every_intermediate_value() {
        let s = suite("GET", "/", &[], &[], EMPTY_SHA256);
        assert_eq!(
            s.canonical_request,
            "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            s.string_to_sign,
            "AWS4-HMAC-SHA256\n20150830T123600Z\n20150830/us-east-1/service/aws4_request\n\
             bb579772317eb040ac9ed261061d46c1f17a8133879d6129b6e1c25292927e63"
        );
        assert_eq!(s.signature, "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31");
        assert_eq!(
            s.authorization,
            authz("host;x-amz-date", "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31")
        );
    }

    #[test]
    fn suite_query_string_cases() {
        // Parameters arrive out of order and are sorted.
        let s = suite("GET", "/", &[("Param2", "value2"), ("Param1", "value1")], &[], EMPTY_SHA256);
        assert!(s.canonical_request.starts_with("GET\n/\nParam1=value1&Param2=value2\n"), "{}", s.canonical_request);
        assert_eq!(s.signature, "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500");

        // Every unreserved character, as key and as value, goes through unescaped.
        let all = "-._~0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let s = suite("GET", "/", &[(all, all)], &[], EMPTY_SHA256);
        assert_eq!(s.signature, "9c3e54bfcdf0b19771a7f523ee5669cdf59bc7cc0884027167c21bb143a40197");

        // A non-ASCII name is encoded as UTF-8 bytes.
        let s = suite("GET", "/", &[("\u{1234}", "bar")], &[], EMPTY_SHA256);
        assert!(s.canonical_request.contains("\n%E1%88%B4=bar\n"));
        assert_eq!(s.signature, "2cdec8eed098649ff3a119c94853b13c643bcf08f8b0a1d91e12c9027818dd04");
    }

    #[test]
    fn suite_path_cases() {
        let all = "-._~0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let s = suite("GET", &format!("/{all}"), &[], &[], EMPTY_SHA256);
        assert_eq!(s.signature, "07ef7494c76fa4850883e2b006601f940f8a34d404d0cfa977f52a65bbf5f24f");

        let s = suite("GET", &uri_encode("/\u{1234}", true), &[], &[], EMPTY_SHA256);
        assert_eq!(s.signature, "8318018e0b0f223aa2bbf98705b62bb787dc9c0e678f255a891fd03141be5d85");
    }

    #[test]
    fn suite_header_value_is_trimmed_and_collapsed() {
        let s = suite("GET", "/", &[], &[("My-Header1", "  value1"), ("MY-HEADER2", "  \"a   b   c\"")], EMPTY_SHA256);
        assert!(s.canonical_request.contains("\nmy-header1:value1\nmy-header2:\"a b c\"\n"), "{}", s.canonical_request);
        assert_eq!(s.signed_headers, "host;my-header1;my-header2;x-amz-date");
        assert_eq!(s.signature, "acc3ed3afb60bb290fc8d2dd0098b9911fcaa05412b367055dee359757a9c736");
    }

    /// The S3 documentation's "GET Object" example (AWS Signature Version 4, authorization header):
    /// the one S3-shaped vector that carries a signed `Range` header and a signed payload hash.
    #[test]
    fn s3_documentation_get_object_with_range() {
        let s = sign(
            &Request {
                method: "GET",
                canonical_uri: "/test.txt",
                query: &[],
                headers: &[
                    ("host", "examplebucket.s3.amazonaws.com"),
                    ("range", "bytes=0-9"),
                    ("x-amz-content-sha256", EMPTY_SHA256),
                    ("x-amz-date", "20130524T000000Z"),
                ],
                payload_hash: EMPTY_SHA256,
            },
            &Scope {
                access_key: "AKIAIOSFODNN7EXAMPLE",
                secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
                region: "us-east-1",
                service: "s3",
                amz_date: "20130524T000000Z",
            },
        );
        assert_eq!(s.signed_headers, "host;range;x-amz-content-sha256;x-amz-date");
        assert_eq!(s.signature, "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    }

    /// The S3 documentation's "GET Bucket (List Objects)" example: a signed query string with two
    /// parameters, given out of order, on a path-less bucket request.
    #[test]
    fn s3_documentation_list_objects_with_a_query() {
        let scope = Scope {
            access_key: "AKIAIOSFODNN7EXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "s3",
            amz_date: "20130524T000000Z",
        };
        let headers = [
            ("host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ];
        let s = sign(
            &Request {
                method: "GET",
                canonical_uri: "/",
                query: &[("prefix", "J"), ("max-keys", "2")],
                headers: &headers,
                payload_hash: EMPTY_SHA256,
            },
            &scope,
        );
        assert_eq!(s.signature, "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7");

        // And the lifecycle example: a bare sub-resource name is `lifecycle=`.
        let s = sign(
            &Request {
                method: "GET",
                canonical_uri: "/",
                query: &[("lifecycle", "")],
                headers: &headers,
                payload_hash: EMPTY_SHA256,
            },
            &scope,
        );
        assert_eq!(s.signature, "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543");
    }

    #[test]
    fn encodes_keys_the_way_s3_wants_them() {
        assert_eq!(uri_encode("a b", true), "a%20b");
        assert_eq!(uri_encode("a+b", true), "a%2Bb");
        assert_eq!(uri_encode("dir/sub dir/file.txt", true), "dir/sub%20dir/file.txt");
        assert_eq!(uri_encode("dir/file", false), "dir%2Ffile");
        assert_eq!(uri_encode("caf\u{e9}", true), "caf%C3%A9");
        assert_eq!(uri_encode("\u{1f600}", true), "%F0%9F%98%80");
        assert_eq!(uri_encode("test$file.text", true), "test%24file.text");
        assert_eq!(uri_encode("~-._", true), "~-._");
        assert_eq!(uri_encode("100%", true), "100%25");
    }

    #[test]
    fn canonical_query_sorts_by_encoded_name_and_keeps_empty_values() {
        assert_eq!(canonical_query(&[("prefix", "J"), ("max-keys", "2")]), "max-keys=2&prefix=J");
        assert_eq!(canonical_query(&[("lifecycle", "")]), "lifecycle=");
        assert_eq!(canonical_query(&[("prefix", "a b/c+d")]), "prefix=a%20b%2Fc%2Bd");
        assert_eq!(canonical_query(&[("b", "2"), ("a", "2"), ("a", "1")]), "a=1&a=2&b=2");
    }

    #[test]
    fn sha256_of_nothing() {
        assert_eq!(sha256_hex(b""), EMPTY_SHA256);
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
