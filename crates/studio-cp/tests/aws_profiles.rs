//! `list_aws_profiles` (studio#104) backs the New Fleet wizard's Credential
//! profile `<select>` — it must return only actual *credential* profiles.
//! `~/.aws/config` mixes in other section types (`[sso-session …]`,
//! `[services …]`, `[preview]`, `[plugins …]`) that configure SSO sessions /
//! service endpoints / preview features; offering one of those as a
//! "credential profile" would have the wizard write a fleets.toml binding
//! whose `profile` resolves to nothing at deploy time.

/// `aws_dir()` resolves `$HOME/.aws` per call, so redirecting HOME for the
/// duration of a test exercises the real file-scanning path. Both tests in
/// this binary mutate HOME, so every mutation runs under `HOME_LOCK`.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn home_lock() -> std::sync::MutexGuard<'static, ()> {
    // A panicking test poisons the mutex; a sibling should still run its own
    // assertions rather than die on the poison.
    HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn with_home<R>(dir: &std::path::Path, f: impl FnOnce() -> R) -> R {
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", dir);
    let r = f();
    match prev {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    r
}

#[test]
fn aws_config_non_profile_sections_are_not_offered_as_profiles() {
    let _guard = home_lock();
    let dir = std::env::temp_dir().join(format!("oab-aws-profiles-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let aws_dir = dir.join(".aws");
    std::fs::create_dir_all(&aws_dir).unwrap();

    // A realistic config mixing credential profiles with other section types.
    let config = r#"
[default]
region = ap-east-2

[profile oab-fleet]
region = us-west-2

[sso-session corp]
sso_start_url = https://corp.awsapps.com/start
sso_region = us-east-1
sso_registration_scopes = sso:account:access

[profile dev]
sso_session = corp
region = us-east-1

[services my-svc]
s3 =
  endpoint_url = https://localhost:4566

[preview]

[plugins something]
cli_legacy_plugin_path = /tmp/x
"#;
    std::fs::write(aws_dir.join("config"), config).unwrap();

    // A credentials-only profile (no [profile …] entry in config) merges in.
    std::fs::write(
        aws_dir.join("credentials"),
        "[legacy-cred]\naws_access_key_id = x\naws_secret_access_key = y\n",
    )
    .unwrap();

    let result = with_home(&dir, studio_cp::list_aws_profiles);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(result.exists, "config+credentials written under $HOME/.aws");
    assert_eq!(result.error, None);

    let names: Vec<&str> = result.profiles.iter().map(|p| p.name.as_str()).collect();
    for wanted in ["default", "oab-fleet", "dev", "legacy-cred"] {
        assert!(
            names.contains(&wanted),
            "expected profile {wanted:?} in {names:?}"
        );
    }
    for bogus in [
        "sso-session corp",
        "services my-svc",
        "preview",
        "plugins something",
    ] {
        assert!(
            !names.contains(&bogus),
            "non-profile section {bogus:?} must not be offered as a profile ({names:?})"
        );
    }

    let region_of = |name: &str| {
        result
            .profiles
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.region.as_deref())
    };
    assert_eq!(region_of("default"), Some("ap-east-2"));
    assert_eq!(region_of("oab-fleet"), Some("us-west-2"));
    // `sso_session = corp` is a key inside [profile dev], not a region.
    assert_eq!(region_of("dev"), Some("us-east-1"));
    assert_eq!(region_of("legacy-cred"), None);
}

/// `exists=false` (and no error) when neither file is present — the console's
/// "run `aws configure`" guidance tier keys off exactly this.
#[test]
fn aws_profiles_missing_files_report_exists_false() {
    let _guard = home_lock();
    let dir = std::env::temp_dir().join(format!("oab-aws-profiles-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let result = with_home(&dir, studio_cp::list_aws_profiles);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!result.exists);
    assert_eq!(result.error, None);
    assert!(result.profiles.is_empty());
}
