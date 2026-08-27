// Run: docker run -p 9000:9000 -e MINIO_ROOT_USER=test -e MINIO_ROOT_PASSWORD=testtest \
//        minio/minio server /data ; then create bucket "git-ark-test"; then:
//      GIT_ARK_S3_ENDPOINT=http://localhost:9000 GIT_ARK_TEST_S3=1 cargo test --test s3_minio -- --ignored
use git_ark::config::{AwsSecrets, S3Config};
use git_ark::s3::S3ObjectStore;
use git_ark::store::ObjectStore;

#[test]
#[ignore]
fn minio_put_get_list_roundtrip() {
    if std::env::var("GIT_ARK_TEST_S3").is_err() {
        return;
    }
    let cfg = S3Config {
        bucket: "git-ark-test".into(),
        region: "us-east-1".into(),
        prefix: "git-ark".into(),
        endpoint: None,
    };
    // Endpoint override for MinIO comes from GIT_ARK_S3_ENDPOINT (see s3.rs).
    let creds = AwsSecrets {
        access_key_id: "test".into(),
        secret_access_key: "testtest".into(),
        session_token: None,
    };
    let s = S3ObjectStore::new(&cfg, &creds).unwrap();
    s.put("git-ark/it/latest.age", b"cipher").unwrap();
    assert_eq!(s.get("git-ark/it/latest.age").unwrap(), b"cipher");
    assert!(s
        .list("git-ark/it/")
        .unwrap()
        .iter()
        .any(|k| k.ends_with("latest.age")));
}
